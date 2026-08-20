-- Uniform facade conformance suite for the Haskell binding.
--
-- Instantiates the family C1-C10 contract for Haskell against a live
-- libdictenstein shared library. It needs only libdictenstein and the canonical
-- fixture, never a liblevenshtein transducer, so it pins the *producer* ABI in
-- isolation.
--
--   C1  identity + kind/capabilities per backend
--   C2  idempotent close + free-order independence
--   C3  IOError raised (+ non-empty message) for INVALID_UTF8/DOMAIN_MISMATCH/
--       IO_ERROR (the facade throws userError with the native message; the
--       numeric status is not carried, so status-code granularity is N/A)
--   C4  canonical fixture replay (all four backends)
--   C5  CRUD + value + batch + substring; capability-derived assertions
--   C6  precomposed/combining/multibyte, byte-domain NUL + invalid UTF-8, u64 0/MAX
--   C7  batch sizes 0/1/255/256/257/1000 (putManyBytes)
--   C8  CRUD op-script vs a Map oracle; substring vs a naive oracle
--   C9  leak discipline (>=10k cycles, RSS bounded)
--   C10 concurrency: independent per-thread dictionaries + readers during a
--       writer (forkIO on the threaded RTS with -N)
module Main (main) where

import Control.Concurrent (forkIO)
import Control.Concurrent.MVar (newEmptyMVar, putMVar, takeMVar)
import Control.Exception (SomeException, try)
import Control.Monad (forM, forM_, when)
import Data.Bits (shiftR)
import qualified Data.ByteString as BS
import Data.Char (chr, digitToInt, isDigit, isSpace)
import Data.IORef (IORef, atomicModifyIORef', newIORef, readIORef, writeIORef)
import Data.List (foldl', isPrefixOf)
import qualified Data.Map.Strict as Map
import Data.Maybe (fromMaybe, isJust)
import Data.Word (Word64)
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import System.Environment (getArgs)
import System.Exit (exitFailure, exitSuccess)
import System.IO (hPutStrLn, stderr)
import VinaryTree.Interop (UnitDomain (..))
import VinaryTree.Libdictenstein

-- --------------------------------------------------------------------------
-- test harness
-- --------------------------------------------------------------------------

check :: IORef Int -> Bool -> String -> IO ()
check failures ok message =
  when (not ok) $ do
    hPutStrLn stderr ("FAIL: " ++ message)
    _ <- atomicModifyIORef' failures (\n -> (n + 1, ()))
    pure ()

bs :: String -> BS.ByteString
bs = TE.encodeUtf8 . T.pack

-- Capability bits (LDICT_CAP_*).
capRead, capInsert, capRemove, capClear, capCompact, capSubstring, capCheckpoint :: Word64
capRead = 1
capInsert = 2
capRemove = 4
capClear = 8
capCompact = 16
capSubstring = 32
capCheckpoint = 64

hasCap :: Word64 -> Word64 -> Bool
hasCap caps bit = (caps `quot` bit) `rem` 2 == 1

-- --------------------------------------------------------------------------
-- minimal JSON parser (objects, arrays, strings, integers, true/false/null)
-- --------------------------------------------------------------------------

data J = JNull | JBool Bool | JNum Integer | JStr String | JArr [J] | JObj [(String, J)]

ws :: String -> String
ws = dropWhile isSpace

parseJSON :: String -> J
parseJSON = fst . pValue . ws

pValue :: String -> (J, String)
pValue s = case s of
  ('{' : r) -> pObj (ws r) []
  ('[' : r) -> pArr (ws r) []
  ('"' : r) -> let (v, r') = pStr r in (JStr v, r')
  ('t' : r) -> (JBool True, drop 3 r)
  ('f' : r) -> (JBool False, drop 4 r)
  ('n' : r) -> (JNull, drop 3 r)
  _ -> let (digits, r) = span (\c -> isDigit c || c == '-') s in (JNum (read digits), r)

pStr :: String -> (String, String)
pStr = go []
  where
    go acc ('"' : r) = (reverse acc, r)
    go acc ('\\' : c : r) = case c of
      'n' -> go ('\n' : acc) r
      't' -> go ('\t' : acc) r
      'r' -> go ('\r' : acc) r
      'u' -> let (h, r') = splitAt 4 r
                 code = foldl' (\a d -> a * 16 + digitToInt d) 0 h
              in go (chr code : acc) r'
      _ -> go (c : acc) r
    go acc (c : r) = go (c : acc) r
    go acc [] = (reverse acc, [])

pObj :: String -> [(String, J)] -> (J, String)
pObj s acc = case ws s of
  ('}' : r) -> (JObj (reverse acc), r)
  ('"' : r0) ->
    let (key, r1) = pStr r0
        r2 = ws r1
        r3 = case r2 of (':' : x) -> ws x; _ -> r2
        (value, r4) = pValue r3
     in case ws r4 of
          (',' : r5) -> pObj (ws r5) ((key, value) : acc)
          ('}' : r5) -> (JObj (reverse ((key, value) : acc)), r5)
          other -> (JObj (reverse ((key, value) : acc)), other)
  other -> (JObj (reverse acc), other)

pArr :: String -> [J] -> (J, String)
pArr s acc = case ws s of
  (']' : r) -> (JArr (reverse acc), r)
  s1 ->
    let (value, r1) = pValue s1
     in case ws r1 of
          (',' : r2) -> pArr (ws r2) (value : acc)
          (']' : r2) -> (JArr (reverse (value : acc)), r2)
          other -> (JArr (reverse (value : acc)), other)

member :: String -> J -> J
member key (JObj o) = fromMaybe (error ("missing key: " ++ key)) (lookup key o)
member key _ = error ("not an object for key: " ++ key)

jArr :: J -> [J]
jArr (JArr a) = a
jArr _ = error "expected array"

jStr :: J -> String
jStr (JStr s) = s
jStr _ = error "expected string"

jBool :: J -> Bool
jBool (JBool b) = b
jBool _ = error "expected bool"

jInt :: J -> Int
jInt (JNum n) = fromInteger n
jInt _ = error "expected number"

jOptVal :: J -> Maybe Word64
jOptVal JNull = Nothing
jOptVal (JNum n) = Just (fromInteger n)
jOptVal _ = error "expected number or null"

-- --------------------------------------------------------------------------
-- main
-- --------------------------------------------------------------------------

main :: IO ()
main = do
  args <- getArgs
  path <- case args of
    (p : _) -> pure p
    [] -> findFixture ["bindings/canonical_fixture.json", "../canonical_fixture.json", "../../canonical_fixture.json"]
  raw <- BS.readFile path
  let root = parseJSON (T.unpack (TE.decodeUtf8 raw))
  failures <- newIORef (0 :: Int)

  c1 failures
  c2 failures
  c3 failures root
  c4 failures root
  c5 failures
  c6 failures
  c7 failures
  c8 failures
  c9 failures
  c10 failures
  entriesConformance failures

  count <- readIORef failures
  if count == 0
    then putStrLn "haskell conformance: all checks passed" >> exitSuccess
    else hPutStrLn stderr ("haskell conformance: " ++ show count ++ " check(s) failed") >> exitFailure

findFixture :: [FilePath] -> IO FilePath
findFixture [] = pure "bindings/canonical_fixture.json"
findFixture (p : rest) = do
  ok <- try (BS.readFile p) :: IO (Either SomeException BS.ByteString)
  case ok of Right _ -> pure p; Left _ -> findFixture rest

entriesOf :: J -> [(T.Text, Maybe Word64)]
entriesOf root =
  [ (T.pack (jStr (member "term" e)), jOptVal (member "value" e)) | e <- jArr (member "entries" root) ]

byteEntriesOf :: J -> [(BS.ByteString, Maybe Word64)]
byteEntriesOf root = [ (TE.encodeUtf8 t, v) | (t, v) <- entriesOf root ]

-- C1 ------------------------------------------------------------------------

c1 :: IORef Int -> IO ()
c1 failures = do
  abi <- abiVersion
  api <- apiRevision
  check failures (abi == 1) "abi version == 1"
  check failures (api == 5) "api revision == 5"
  dawg <- dynamicDawg UnicodeScalar
  k <- dictionaryKind dawg
  check failures (k == 1) "dawg kind"
  caps <- capabilities dawg
  check failures (hasCap caps capInsert && hasCap caps capRemove && hasCap caps capClear && hasCap caps capCompact) "dawg caps"
  check failures (not (hasCap caps capSubstring) && not (hasCap caps capCheckpoint)) "dawg lacks substring/checkpoint"
  close dawg
  dat <- doubleArrayTrie UnicodeScalar [(bs "x", Nothing)]
  dk <- dictionaryKind dat
  dc <- capabilities dat
  check failures (dk == 2) "dat kind"
  check failures (hasCap dc capRead) "dat read"
  close dat
  sc <- scdawg UnicodeScalar
  sk <- dictionaryKind sc
  scaps <- capabilities sc
  check failures (sk == 3) "scdawg kind"
  check failures (hasCap scaps capSubstring) "scdawg substring"
  close sc

-- C2 ------------------------------------------------------------------------

c2 :: IORef Int -> IO ()
c2 _ = do
  dawg <- dynamicDawg UnicodeScalar
  _ <- putText dawg (T.pack "a") Nothing
  close dawg
  close dawg -- idempotent
  dawgs <- forM [0 .. 3] $ \i -> do
    d <- dynamicDawg UnicodeScalar
    _ <- putText d (T.pack ("term" ++ show i)) (Just (fromIntegral i))
    pure d
  forM_ [2, 0, 3, 1] $ \i -> close (dawgs !! i)

-- C3 ------------------------------------------------------------------------

raises :: IO a -> IO (Bool, String)
raises action = do
  result <- try action
  pure $ case result of
    Left e -> (True, show (e :: SomeException))
    Right _ -> (False, "")

c3 :: IORef Int -> J -> IO ()
c3 failures _ = do
  dawg <- dynamicDawg UnicodeScalar
  (r1, m1) <- raises (putBytes dawg (BS.pack [0xff]) Nothing)
  check failures (r1 && not (null m1)) "invalid utf8 raises with message"
  (r2, _) <- raises (putU64 dawg [1, 2] Nothing)
  check failures r2 "domain mismatch raises"
  close dawg
  (r3, m3) <- raises (openPersistentARTrie UnicodeScalar "/nonexistent/ldict-hs-missing.part")
  check failures (r3 && not (null m3)) "io error raises with message"

-- C4 ------------------------------------------------------------------------

assertFixtureReads :: IORef Int -> J -> Dictionary -> IO ()
assertFixtureReads failures root dictionary = do
  n <- dictionaryLength dictionary
  check failures (n == jInt (member "size" root)) "fixture size"
  forM_ (jArr (member "contains" root)) $ \c -> do
    let term = jStr (member "term" c)
    present <- containsText dictionary (T.pack term)
    check failures (present == jBool (member "expected" c)) ("contains " ++ term)
  forM_ (jArr (member "get" root)) $ \g -> do
    let term = jStr (member "term" g)
    hit <- getText dictionary (T.pack term)
    check failures (found hit == jBool (member "found" g)) ("get.found " ++ term)
    check failures (mappedValue hit == jOptVal (member "value" g)) ("get.value " ++ term)

c4 :: IORef Int -> J -> IO ()
c4 failures root = do
  dawg <- dynamicDawg UnicodeScalar
  inserted <- putManyBytes dawg (byteEntriesOf root)
  check failures (inserted == jInt (member "size" root)) "dawg batch count"
  assertFixtureReads failures root dawg
  close dawg
  dat <- doubleArrayTrie UnicodeScalar (byteEntriesOf root)
  assertFixtureReads failures root dat
  close dat
  let path = "/tmp/ldict-hs-c4-" ++ "fixture.part"
  art <- createPersistentARTrie UnicodeScalar path
  forM_ (entriesOf root) $ \(t, v) -> putText art t v >> pure ()
  assertFixtureReads failures root art
  close art
  sc <- scdawg UnicodeScalar
  forM_ (entriesOf root) $ \(t, v) -> putText sc t v >> pure ()
  forM_ (jArr (member "substring_frequency" root)) $ \c -> do
    let pattern = jStr (member "pattern" c)
    freq <- substringFrequency sc (bs pattern)
    check failures (freq == jInt (member "expected" c)) ("frequency " ++ pattern)
  forM_ (jArr (member "substring_contains" root)) $ \c -> do
    let pattern = jStr (member "pattern" c)
    present <- containsSubstring sc (bs pattern)
    check failures (present == jBool (member "expected" c)) ("contains_substring " ++ pattern)
  close sc

-- C5 ------------------------------------------------------------------------

c5 :: IORef Int -> IO ()
c5 failures = do
  dawg <- dynamicDawg UnicodeScalar
  a <- putText dawg (T.pack "cat") (Just 1)
  check failures a "insert cat"
  b <- putText dawg (T.pack "cat") (Just 1)
  check failures (not b) "idempotent insert"
  hit <- getText dawg (T.pack "cat")
  check failures (mappedValue hit == Just 1) "get cat"
  r1 <- removeText dawg (T.pack "cat")
  check failures r1 "remove cat"
  r2 <- removeText dawg (T.pack "cat")
  check failures (not r2) "second remove"
  present <- containsText dawg (T.pack "cat")
  check failures (not present) "cat gone"
  forM_ [0 .. 49] $ \i -> putText dawg (T.pack ("t" ++ show i)) (Just (fromIntegral i)) >> pure ()
  forM_ [0, 2 .. 48] $ \i -> do
    ok <- removeText dawg (T.pack ("t" ++ show i))
    check failures ok ("remove even t" ++ show i)
  _ <- compact dawg
  n <- dictionaryLength dawg
  check failures (n == 25) "compact size"
  survives <- getText dawg (T.pack "t1")
  check failures (mappedValue survives == Just 1) "t1 survives"
  gone <- containsText dawg (T.pack "t0")
  check failures (not gone) "t0 gone"
  close dawg
  sc <- scdawg UnicodeScalar
  _ <- putText sc (T.pack "cat") (Just 1)
  _ <- putText sc (T.pack "cot") (Just 2)
  f2 <- substringFrequency sc (bs "t")
  check failures (f2 == 2) "freq t == 2"
  ins <- putText sc (T.pack "cut") Nothing
  check failures ins "insert cut"
  f3 <- substringFrequency sc (bs "t")
  check failures (f3 == 3) "freq t == 3"
  close sc
  dat <- doubleArrayTrie UnicodeScalar [(bs "x", Nothing)]
  caps <- capabilities dat
  check failures (not (hasCap caps capInsert) && not (hasCap caps capRemove) && not (hasCap caps capClear) && not (hasCap caps capCompact)) "dat capability-derived reject"
  close dat

-- C6 ------------------------------------------------------------------------

c6 :: IORef Int -> IO ()
c6 failures = do
  dawg <- dynamicDawg UnicodeScalar
  p1 <- putText dawg (T.pack "caf\x00e9") (Just 7) -- café, precomposed U+00E9
  check failures p1 "precomposed insert"
  p2 <- putText dawg (T.pack "\x1F980") (Just 255) -- crab, 4-byte scalar
  check failures p2 "emoji insert"
  cont <- containsText dawg (T.pack "caf\x00e9")
  check failures cont "precomposed contains"
  hit <- getText dawg (T.pack "\x1F980")
  check failures (mappedValue hit == Just 255) "emoji value"
  close dawg
  dawg2 <- dynamicDawg UnicodeScalar
  let precomposed = T.pack "caf\x00e9" -- café, precomposed U+00E9
      combining = T.pack "cafe\x0301" -- cafe + U+0301 combining acute
  _ <- putText dawg2 precomposed (Just 1)
  _ <- putText dawg2 combining (Just 2)
  n <- dictionaryLength dawg2
  check failures (n == 2) "distinct scalar sequences"
  h1 <- getText dawg2 precomposed
  h2 <- getText dawg2 combining
  check failures (mappedValue h1 == Just 1 && mappedValue h2 == Just 2) "distinct values"
  close dawg2
  bdawg <- dynamicDawg Byte
  bn <- putBytes bdawg (BS.pack [0x61, 0x00, 0x62]) (Just 1)
  check failures bn "embedded NUL insert"
  bi <- putBytes bdawg (BS.pack [0xff, 0xfe]) (Just 2)
  check failures bi "invalid utf8 byte insert"
  bc <- containsBytes bdawg (BS.pack [0x61, 0x00, 0x62])
  check failures bc "embedded NUL contains"
  bh <- getBytes bdawg (BS.pack [0xff, 0xfe])
  check failures (mappedValue bh == Just 2) "invalid utf8 byte value"
  close bdawg
  udawg <- dynamicDawg U64
  _ <- putU64 udawg [1, 2, 3] (Just 0)
  _ <- putU64 udawg [9] (Just maxBound)
  z <- getU64 udawg [1, 2, 3]
  m <- getU64 udawg [9]
  check failures (mappedValue z == Just 0) "u64 value 0"
  check failures (mappedValue m == Just maxBound) "u64 value MAX"
  close udawg

-- C7 ------------------------------------------------------------------------

c7 :: IORef Int -> IO ()
c7 failures =
  forM_ [0, 1, 255, 256, 257, 1000] $ \size -> do
    dawg <- dynamicDawg UnicodeScalar
    let batch = [(bs ("t" ++ show i), Just (fromIntegral i)) | i <- [0 .. size - 1]]
    inserted <- putManyBytes dawg batch
    check failures (inserted == size) ("batch " ++ show size ++ " count")
    n <- dictionaryLength dawg
    check failures (n == size) ("batch " ++ show size ++ " size")
    when (size > 0) $ do
      first <- getText dawg (T.pack "t0")
      lastHit <- getText dawg (T.pack ("t" ++ show (size - 1)))
      check failures (mappedValue first == Just 0) ("batch " ++ show size ++ " first")
      check failures (mappedValue lastHit == Just (fromIntegral (size - 1))) ("batch " ++ show size ++ " last")
    close dawg

-- C8 ------------------------------------------------------------------------

lcg :: Word64 -> Word64
lcg s = s * 6364136223846793005 + 1442695040888963407

nextInt :: IORef Word64 -> Int -> IO Int
nextInt ref n = do
  s <- readIORef ref
  let s' = lcg s
  writeIORef ref s'
  pure (fromIntegral ((s' `shiftR` 33) `mod` fromIntegral n))

c8 :: IORef Int -> IO ()
c8 failures = do
  crudScript failures
  substringNaive failures

crudScript :: IORef Int -> IO ()
crudScript failures = do
  rng <- newIORef (0xC0FFEE :: Word64)
  oracle <- newIORef (Map.empty :: Map.Map String (Maybe Word64))
  dawg <- dynamicDawg UnicodeScalar
  let keys = ["k" ++ show i | i <- [0 .. 39 :: Int]]
  forM_ [1 .. 3000 :: Int] $ \_ -> do
    ki <- nextInt rng 40
    let key = keys !! ki
    o <- readIORef oracle
    let present = Map.member key o
    op <- nextInt rng 100
    if op < 50
      then do
        coin <- nextInt rng 2
        v <- if coin == 0 then pure Nothing else Just . fromIntegral <$> nextInt rng 1000000000
        changed <- putText dawg (T.pack key) v
        check failures (changed == not present) "crud insert changed"
        writeIORef oracle (Map.insert key v o)
      else
        if op < 75
          then do
            changed <- removeText dawg (T.pack key)
            check failures (changed == present) "crud remove changed"
            writeIORef oracle (Map.delete key o)
          else
            if op < 95
              then do
                got <- containsText dawg (T.pack key)
                check failures (got == present) "crud contains"
                when present $ do
                  hit <- getText dawg (T.pack key)
                  check failures (mappedValue hit == fromMaybe Nothing (Map.lookup key o)) "crud get value"
              else compact dawg >> pure ()
    o' <- readIORef oracle
    n <- dictionaryLength dawg
    check failures (n == Map.size o') "crud size matches oracle"
  close dawg

substringNaive :: IORef Int -> IO ()
substringNaive failures = do
  rng <- newIORef (0x5CDA :: Word64)
  let alphabet = "abcx"
      gen maxLen = do
        n <- nextInt rng maxLen
        chars <- forM [0 .. n] $ \_ -> do
          k <- nextInt rng (length alphabet)
          pure (alphabet !! k)
        pure chars
      collect acc
        | Map.size acc >= 60 = pure (Map.keys acc)
        | otherwise = do t <- gen 5; collect (Map.insert t () acc)
  terms <- collect (Map.empty :: Map.Map String ())
  let naive pattern = sum [countOcc term pattern | term <- terms]
      countOcc term pattern =
        length [() | i <- [0 .. length term - length pattern], take (length pattern) (drop i term) == pattern]
  sc <- scdawg UnicodeScalar
  forM_ terms $ \t -> putText sc (T.pack t) Nothing >> pure ()
  forM_ [1 .. 200 :: Int] $ \_ -> do
    pattern <- gen 2
    let expected = naive pattern
    freq <- substringFrequency sc (bs pattern)
    check failures (freq == expected) ("pbt frequency " ++ pattern)
    present <- containsSubstring sc (bs pattern)
    check failures (present == (expected > 0)) ("pbt contains " ++ pattern)
  close sc

-- C9 ------------------------------------------------------------------------

rssKib :: IO Int
rssKib = do
  result <- try (BS.readFile "/proc/self/status") :: IO (Either SomeException BS.ByteString)
  case result of
    Left _ -> pure 0
    Right content ->
      let ls = lines (T.unpack (TE.decodeUtf8 content))
       in case filter ("VmRSS:" `isPrefixOf`) ls of
            (l : _) -> pure (read (takeWhile isDigit (dropWhile (not . isDigit) l)))
            [] -> pure 0

c9 :: IORef Int -> IO ()
c9 failures = do
  let cycles = 12000 :: Int
      batch = [(bs "cat", Just 1), (bs "cot", Just 2), (bs "cut", Nothing)]
  forM_ [1 .. 2000 :: Int] $ \_ -> do
    d <- dynamicDawg UnicodeScalar
    _ <- putText d (T.pack "cat") (Just 1)
    close d
  before <- rssKib
  forM_ [1 .. cycles] $ \_ -> do
    d <- dynamicDawg UnicodeScalar
    _ <- putManyBytes d batch
    present <- containsText d (T.pack "cot")
    check failures present "leak cycle contains"
    close d
  after <- rssKib
  when (before > 0 && after > before) $
    check failures (after - before < 64 * 1024) ("RSS grew " ++ show (after - before) ++ " KiB over " ++ show cycles ++ " cycles")

-- C10 -----------------------------------------------------------------------

c10 :: IORef Int -> IO ()
c10 failures = do
  independent failures
  readersDuringWriter failures

independent :: IORef Int -> IO ()
independent failures = do
  dones <- forM [0 .. 7 :: Int] $ \seed -> do
    done <- newEmptyMVar
    _ <- forkIO $ do
      result <- try $ do
        d <- dynamicDawg UnicodeScalar
        forM_ [0 .. 1999 :: Int] $ \i -> putText d (T.pack ("t" ++ show seed ++ "_" ++ show i)) (Just (fromIntegral i)) >> pure ()
        n <- dictionaryLength d
        hit <- getText d (T.pack ("t" ++ show seed ++ "_1500"))
        close d
        pure (n == 2000 && mappedValue hit == Just 1500)
      putMVar done (either (const False) id (result :: Either SomeException Bool))
    pure done
  oks <- mapM takeMVar dones
  check failures (and oks) "independent per-thread dictionaries"

readersDuringWriter :: IORef Int -> IO ()
readersDuringWriter failures = do
  dawg <- dynamicDawg UnicodeScalar
  _ <- putManyBytes dawg [(bs ("seed" ++ show i), Just (fromIntegral i)) | i <- [0 .. 499 :: Int]]
  stop <- newIORef False
  dones <- forM [0 .. 3 :: Int] $ \_ -> do
    done <- newEmptyMVar
    _ <- forkIO $ do
      result <- try $ do
        let loop = do
              s <- readIORef stop
              if s
                then pure True
                else do
                  present <- containsText dawg (T.pack "seed0")
                  _ <- getText dawg (T.pack "seed250")
                  if present then loop else pure False
        loop
      putMVar done (either (const False) id (result :: Either SomeException Bool))
    pure done
  forM_ [500 .. 2999 :: Int] $ \i -> putText dawg (T.pack ("w" ++ show i)) (Just (fromIntegral i)) >> pure ()
  writeIORef stop True
  oks <- mapM takeMVar dones
  check failures (and oks) "concurrent readers during writer"
  final <- getText dawg (T.pack "w2999")
  check failures (mappedValue final == Just 2999) "final write present"
  close dawg

-- entries-v1 ---------------------------------------------------------------

entriesConformance :: IORef Int -> IO ()
entriesConformance failures = do
  unicode <- dynamicDawg UnicodeScalar
  _ <- putText unicode (T.pack "cat") Nothing
  _ <- putText unicode (T.pack "caf\233") (Just maxBound)
  _ <- putText unicode T.empty (Just 0)
  captured <- withEntryStream unicode $ \stream -> do
    let metadata = entryMetadata stream
    check failures (entriesUnitDomain metadata == UnicodeScalar) "entries unicode domain"
    check failures (entriesHaveOptionalValues metadata) "entries optional-u64 value domain"
    check failures (entriesExactLength metadata == Just 3) "entries exact length"
    check failures (isJust (entriesSnapshotIdentity metadata)) "entries snapshot identity"
    _ <- putText unicode (T.pack "dog") (Just 4)
    let pull reversed = nextEntry stream >>= maybe (pure (reverse reversed))
          (\entry -> pull (entry : reversed))
    pull []
  check failures
    (captured ==
      [ DictionaryEntry (UnicodeKey T.empty) (Just 0)
      , DictionaryEntry (UnicodeKey (T.pack "caf\233")) (Just maxBound)
      , DictionaryEntry (UnicodeKey (T.pack "cat")) Nothing
      ])
    "entries immutable lexicographic Unicode snapshot"
  current <- materializeEntries unicode
  check failures (length (snapshotEntries current) == 4) "entries later revision sees mutation"
  early <- try (withEntryStream unicode $ \stream -> do
    _ <- nextEntry stream
    ioError (userError "intentional early entries exit")) :: IO (Either SomeException ())
  check failures (either (const True) (const False) early) "entries close after exception"
  stillReadable <- containsText unicode (T.pack "cat")
  check failures stillReadable "dictionary usable after early entries close"
  close unicode

  bytesDictionary <- dynamicDawg Byte
  _ <- putBytes bytesDictionary BS.empty Nothing
  _ <- putBytes bytesDictionary (BS.pack [0, 255]) (Just maxBound)
  _ <- putBytes bytesDictionary (BS.pack [1]) (Just 1)
  byteSnapshot <- materializeEntries bytesDictionary
  check failures
    (snapshotEntries byteSnapshot ==
      [ DictionaryEntry (ByteKey BS.empty) Nothing
      , DictionaryEntry (ByteKey (BS.pack [0, 255])) (Just maxBound)
      , DictionaryEntry (ByteKey (BS.pack [1])) (Just 1)
      ])
    "entries preserve arbitrary bytes and byte order"
  close bytesDictionary

  u64Dictionary <- dynamicDawg U64
  _ <- putU64 u64Dictionary [maxBound] (Just maxBound)
  _ <- putU64 u64Dictionary [0] Nothing
  _ <- putU64 u64Dictionary [maxBound `quot` 2 + 1] (Just 0)
  u64Snapshot <- materializeEntries u64Dictionary
  check failures
    (snapshotEntries u64Snapshot ==
      [ DictionaryEntry (U64Key [0]) Nothing
      , DictionaryEntry (U64Key [maxBound `quot` 2 + 1]) (Just 0)
      , DictionaryEntry (U64Key [maxBound]) (Just maxBound)
      ])
    "entries preserve full-width u64 keys and values"
  close u64Dictionary

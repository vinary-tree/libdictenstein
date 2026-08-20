-- Public-package collection traversal benchmark. Dictionary construction and
-- warmup are outside the timed interval; stdout is one v1 JSON sample.
module Main (main) where

import Control.Exception (bracket)
import Control.Monad (replicateM_)
import Data.Bits (xor)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Char8 as BSC
import Data.List (sortOn)
import Data.Word (Word64)
import GHC.Clock (getMonotonicTimeNSec)
import Numeric (showHex)
import System.Environment (getArgs)
import System.Exit (die)
import Text.Printf (printf)
import VinaryTree.Libdictenstein

data Config = Config
  { arm :: String, entryCount :: Int, passes :: Int, warmups :: Int
  , batchSize :: Int, earlyCancel :: Int
  }

defaults :: Config
defaults = Config "" 65536 1 1 256 64

readInt :: String -> String -> IO Int
readInt name value = case reads value of
  [(number, "")] -> pure number
  _ -> die (name ++ " must be an integer")

parse :: Config -> [String] -> IO Config
parse config [] = pure config
parse config (name:value:rest) = do
  next <- case name of
    "--arm" -> pure config { arm = value }
    "--entries" -> (\number -> config { entryCount = number }) <$> readInt name value
    "--passes" -> (\number -> config { passes = number }) <$> readInt name value
    "--warmup-passes" -> (\number -> config { warmups = number }) <$> readInt name value
    "--batch-size" -> (\number -> config { batchSize = number }) <$> readInt name value
    "--early-cancel" -> (\number -> config { earlyCancel = number }) <$> readInt name value
    _ -> die ("unknown argument " ++ name)
  parse next rest
parse _ [_] = die "incomplete argument"

padHex :: Int -> Int -> String
padHex width value = replicate (width - length rendered) '0' ++ rendered
  where rendered = showHex value ""

makeCorpus :: Int -> [(BS.ByteString, Maybe Word64)]
makeCorpus count =
  [ (BSC.pack ("collection/" ++ padHex 4 (index `mod` 4096) ++ "/"
      ++ padHex 8 index ++ "/shared-suffix"), Just (fromIntegral index))
  | index <- [0 .. count - 1]
  ]

entryChecksum :: DictionaryEntry -> IO Word64
entryChecksum (DictionaryEntry (ByteKey key) value) =
  pure (fromIntegral (BS.length key) `xor` maybe 0 id value)
entryChecksum _ = die "benchmark expected byte-domain entries"

drainStream :: EntryBatchLimits -> Dictionary -> Int -> IO (Word64, Int)
drainStream limits dictionary limit = withEntryStreamLimits limits dictionary $ \stream ->
  let loop checksum count
        | count == limit = pure (checksum, count)
        | otherwise = nextEntry stream >>= maybe (pure (checksum, count))
            (\entry -> entryChecksum entry >>= \item -> loop (checksum + item) (count + 1))
  in loop 0 0

main :: IO ()
main = do
  config <- parse defaults =<< getArgs
  if arm config `notElem` ["materialized", "stream", "stream-cancel", "reduce"]
    then die "--arm must be materialized, stream, stream-cancel, or reduce" else pure ()
  if entryCount config <= 0 || passes config <= 0 || batchSize config <= 0
      || earlyCancel config <= 0 || warmups config < 0
    then die "invalid non-positive benchmark argument" else pure ()
  let corpus = makeCorpus (entryCount config)
      ordered = sortOn fst corpus
      consumed = if arm config == "stream-cancel"
        then min (entryCount config) (earlyCancel config) else entryCount config
      expected = sum
        [ fromIntegral (BS.length key) `xor` maybe 0 id value
        | (key, value) <- take consumed ordered ]
      limits = EntryBatchLimits (batchSize config) (batchSize config * 38) (batchSize config)
  bracket (dynamicDawg Byte) close $ \dictionary -> do
    inserted <- putManyBytes dictionary corpus
    if inserted /= entryCount config then die "generated corpus insertion was incomplete" else pure ()
    let drain = case arm config of
          "materialized" -> do
            snapshot <- materializeEntries dictionary
            checksum <- sum <$> mapM entryChecksum (snapshotEntries snapshot)
            pure (checksum, length (snapshotEntries snapshot))
          "reduce" -> foldEntriesWithLimits limits dictionary
            (\(checksum, count) entry -> entryChecksum entry >>= \item ->
              pure (checksum + item, count + 1)) (0, 0)
          _ -> drainStream limits dictionary consumed
        checked = do
          result@(checksum, count) <- drain
          if checksum == expected && count == consumed then pure result
          else die "collection traversal checksum/cardinality mismatch"
    replicateM_ (warmups config) checked
    started <- getMonotonicTimeNSec
    checksum <- let loop 0 total = pure total
                    loop remaining total = checked >>= \(value, _) -> loop (remaining - 1) (total + value)
                in loop (passes config) 0
    finished <- getMonotonicTimeNSec
    let elapsed = max 1 (finished - started)
        batch = if arm config == "materialized" then "null" else show (batchSize config)
        early = if arm config == "stream-cancel" then show (earlyCancel config) else "null"
    putStrLn (printf
      "{\"schema\":\"libdictenstein.host-collection-traversal.v1\",\"runtime\":\"haskell\",\"arm\":\"%s\",\"dictionary_entries\":%d,\"consumed_entries_per_pass\":%d,\"passes\":%d,\"warmup_passes\":%d,\"batch_size\":%s,\"early_cancel\":%s,\"elapsed_ns\":%d,\"checksum\":%d}"
      (arm config) (entryCount config) consumed (passes config) (warmups config)
      batch early elapsed checksum)

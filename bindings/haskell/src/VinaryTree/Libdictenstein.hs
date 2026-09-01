{-# LANGUAGE CApiFFI #-}
{-# LANGUAGE DerivingStrategies #-}
module VinaryTree.Libdictenstein
  ( Dictionary
  , UnitDomain(..)
  , Lookup(..)
  , AlgebraOperation(..)
  , ValueMerge(..)
  , EntryKey(..)
  , DictionaryEntry(..)
  , EntriesMetadata(..)
  , EntryBatchLimits(..)
  , defaultEntryBatchLimits
  , EntryStream
  , entryMetadata
  , nextEntry
  , DictionarySnapshot(..)
  , abiVersion
  , apiRevision
  , dynamicDawg
  , doubleArrayTrie
  , scdawg
  , createPersistentARTrie
  , openPersistentARTrie
  , createPersistentVocabulary
  , openPersistentVocabulary
  , close
  , resource
  , dictionaryLength
  , dictionaryKind
  , capabilities
  , putBytes
  , putText
  , putManyBytes
  , removeBytes
  , removeText
  , containsBytes
  , containsText
  , getBytes
  , getText
  , putU64
  , removeU64
  , containsU64
  , getU64
  , clear
  , compact
  , algebra
  , union
  , intersection
  , difference
  , symmetricDifference
  , checkpoint
  , containsSubstring
  , substringFrequency
  , vocabularyTerm
  , withEntryStream
  , withEntryStreamLimits
  , foldEntries
  , foldEntriesWithLimits
  , materializeEntries
  ) where

import Control.Exception (bracket, onException, throwIO)
import Control.Monad ((>=>))
import Data.Char (chr)
import Data.ByteString (ByteString)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Unsafe as BSU
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Data.Text.Encoding as TextEncoding
import Data.Word (Word8, Word32, Word64)
import Foreign.C.String (peekCString)
import Foreign.C.Types (CChar, CInt(..), CSize(..))
import Foreign.ForeignPtr
  (ForeignPtr, finalizeForeignPtr, newForeignPtr, withForeignPtr)
import Foreign.Marshal.Alloc (alloca, allocaBytes)
import Foreign.Marshal.Array (peekArray, withArray)
import Foreign.Ptr (FunPtr, Ptr, castPtr, nullPtr)
import Foreign.Storable (Storable, peek, poke)
import VinaryTree.Interop
  (DictionaryResource, UnitDomain(..), VtResource, fromOwnedResource)

data LdictDictionary
data LdictEntryCursor
newtype Dictionary = Dictionary (ForeignPtr LdictDictionary)
data Lookup = Lookup { found :: !Bool, mappedValue :: !(Maybe Word64) }
  deriving stock (Eq, Show)
data AlgebraOperation = Union | Intersection | Difference | SymmetricDifference
  deriving stock (Eq, Show)
data ValueMerge = First | Last | LatticeJoin | LatticeMeet
  deriving stock (Eq, Show)
data EntryKey
  = ByteKey !ByteString
  | UnicodeKey !Text
  | U64Key ![Word64]
  deriving stock (Eq, Show)
data DictionaryEntry = DictionaryEntry
  { entryKey :: !EntryKey
  , entryValue :: !(Maybe Word64)
  } deriving stock (Eq, Show)
data EntriesMetadata = EntriesMetadata
  { entriesUnitDomain :: !UnitDomain
  , entriesHaveOptionalValues :: !Bool
  , entriesExactLength :: !(Maybe Word64)
  , entriesSnapshotIdentity :: !(Maybe (Word64, Word64))
  } deriving stock (Eq, Show)
data EntryBatchLimits = EntryBatchLimits
  { maxBatchEntries :: !Int
  , maxBatchUnits :: !Int
  , maxBatchValues :: !Int
  } deriving stock (Eq, Show)
defaultEntryBatchLimits :: EntryBatchLimits
defaultEntryBatchLimits = EntryBatchLimits 256 65536 256
data EntryStream = EntryStream
  { entryMetadata :: !EntriesMetadata
  , entryCursor :: !(Ptr LdictEntryCursor)
  }
data DictionarySnapshot a = DictionarySnapshot
  { snapshotMetadata :: !EntriesMetadata
  , snapshotEntries :: ![a]
  } deriving stock (Eq, Show)

instance Foldable DictionarySnapshot where
  foldMap action = foldMap action . snapshotEntries

type Status = CInt

foreign import ccall unsafe "ldict_abi_version"
  cAbiVersion :: IO Word32
foreign import ccall unsafe "ldict_api_revision"
  cApiRevision :: IO Word32
foreign import ccall unsafe "ldict_last_error_message"
  cLastError :: IO (Ptr CChar)
foreign import ccall unsafe "&ldict_dictionary_free"
  cDictionaryFree :: FunPtr (Ptr LdictDictionary -> IO ())
foreign import ccall unsafe "ldict_dynamic_dawg_new"
  cDynamic :: Word32 -> Ptr (Ptr LdictDictionary) -> IO Status
foreign import ccall unsafe "ldict_scdawg_new"
  cScdawg :: Word32 -> Ptr (Ptr LdictDictionary) -> IO Status
foreign import ccall unsafe "ldict_hs_double_array_trie_new"
  cDat :: Word32 -> Ptr (Ptr Word8) -> Ptr CSize -> Ptr Word64 -> Ptr Word8
       -> CSize -> Ptr (Ptr LdictDictionary) -> IO Status
foreign import ccall safe "ldict_persistent_artrie_create"
  cPersistentCreate :: Word32 -> Ptr Word8 -> CSize
                    -> Ptr (Ptr LdictDictionary) -> IO Status
foreign import ccall safe "ldict_persistent_artrie_open"
  cPersistentOpen :: Word32 -> Ptr Word8 -> CSize
                  -> Ptr (Ptr LdictDictionary) -> IO Status
foreign import ccall safe "ldict_persistent_vocab_create"
  cVocabCreate :: Ptr Word8 -> CSize -> Ptr (Ptr LdictDictionary) -> IO Status
foreign import ccall safe "ldict_persistent_vocab_open"
  cVocabOpen :: Ptr Word8 -> CSize -> Ptr (Ptr LdictDictionary) -> IO Status
foreign import ccall unsafe "ldict_hs_resource"
  cResource :: Ptr LdictDictionary -> Ptr Status -> IO (Ptr VtResource)
foreign import ccall unsafe "ldict_dictionary_len"
  cLength :: Ptr LdictDictionary -> Ptr CSize -> IO Status
foreign import ccall unsafe "ldict_dictionary_kind"
  cKind :: Ptr LdictDictionary -> Ptr Word32 -> IO Status
foreign import ccall unsafe "ldict_dictionary_capabilities"
  cCapabilities :: Ptr LdictDictionary -> Ptr Word64 -> IO Status
foreign import ccall unsafe "ldict_dictionary_insert_text_value"
  cPutText :: Ptr LdictDictionary -> Ptr Word8 -> CSize -> Word64 -> Word8
           -> Ptr Word8 -> IO Status
foreign import ccall unsafe "ldict_hs_insert_text_batch"
  cPutBatch :: Ptr LdictDictionary -> Ptr (Ptr Word8) -> Ptr CSize
            -> Ptr Word64 -> Ptr Word8 -> CSize -> Ptr CSize -> IO Status
foreign import ccall unsafe "ldict_dictionary_remove_text"
  cRemoveText :: Ptr LdictDictionary -> Ptr Word8 -> CSize -> Ptr Word8 -> IO Status
foreign import ccall unsafe "ldict_dictionary_contains_text"
  cContainsText :: Ptr LdictDictionary -> Ptr Word8 -> CSize -> Ptr Word8 -> IO Status
foreign import ccall unsafe "ldict_dictionary_get_text_value"
  cGetText :: Ptr LdictDictionary -> Ptr Word8 -> CSize -> Ptr Word8
           -> Ptr Word64 -> Ptr Word8 -> IO Status
foreign import ccall unsafe "ldict_dictionary_insert_u64_value"
  cPutU64 :: Ptr LdictDictionary -> Ptr Word64 -> CSize -> Word64 -> Word8
          -> Ptr Word8 -> IO Status
foreign import ccall unsafe "ldict_dictionary_remove_u64"
  cRemoveU64 :: Ptr LdictDictionary -> Ptr Word64 -> CSize -> Ptr Word8 -> IO Status
foreign import ccall unsafe "ldict_dictionary_contains_u64"
  cContainsU64 :: Ptr LdictDictionary -> Ptr Word64 -> CSize -> Ptr Word8 -> IO Status
foreign import ccall unsafe "ldict_dictionary_get_u64_value"
  cGetU64 :: Ptr LdictDictionary -> Ptr Word64 -> CSize -> Ptr Word8
          -> Ptr Word64 -> Ptr Word8 -> IO Status
foreign import ccall unsafe "ldict_dictionary_clear"
  cClear :: Ptr LdictDictionary -> IO Status
foreign import ccall unsafe "ldict_dictionary_compact"
  cCompact :: Ptr LdictDictionary -> Ptr CSize -> IO Status
foreign import ccall unsafe "ldict_dictionary_algebra"
  cAlgebra :: Ptr LdictDictionary -> Ptr LdictDictionary -> Word32 -> Word32
           -> Ptr (Ptr LdictDictionary) -> IO Status
foreign import ccall safe "ldict_dictionary_checkpoint"
  cCheckpoint :: Ptr LdictDictionary -> IO Status
foreign import ccall unsafe "ldict_scdawg_contains_substring"
  cSubstring :: Ptr LdictDictionary -> Ptr Word8 -> CSize -> Ptr Word8 -> IO Status
foreign import ccall unsafe "ldict_scdawg_substring_frequency"
  cFrequency :: Ptr LdictDictionary -> Ptr Word8 -> CSize -> Ptr CSize -> IO Status
foreign import ccall unsafe "ldict_vocab_get_term"
  cVocabTerm :: Ptr LdictDictionary -> Word64 -> Ptr Word8 -> CSize
             -> Ptr CSize -> Ptr Word8 -> IO Status
foreign import ccall unsafe "ldict_hs_entries_open"
  cEntriesOpen :: Ptr LdictDictionary -> CSize -> CSize -> CSize
               -> Ptr Status -> IO (Ptr LdictEntryCursor)
foreign import ccall unsafe "ldict_hs_entries_next"
  cEntriesNext :: Ptr LdictEntryCursor -> Ptr (Ptr ()) -> Ptr CSize
               -> Ptr Word64 -> Ptr Word8 -> Ptr Word8 -> IO Status
foreign import ccall unsafe "ldict_hs_entries_info"
  cEntriesInfo :: Ptr LdictEntryCursor -> Ptr Word32 -> Ptr Word32
               -> Ptr Word8 -> Ptr CSize -> Ptr Word8 -> Ptr Word64
               -> Ptr Word64 -> IO ()
foreign import ccall unsafe "ldict_hs_entries_error"
  cEntriesError :: Ptr LdictEntryCursor -> IO (Ptr CChar)
foreign import ccall unsafe "ldict_hs_entries_close"
  cEntriesClose :: Ptr LdictEntryCursor -> IO Status
foreign import ccall unsafe "ldict_hs_entries_free"
  cEntriesFree :: Ptr LdictEntryCursor -> IO ()

-- | Native ABI version (LDICT_ABI_VERSION); always 1 for this family.
abiVersion :: IO Word32
abiVersion = cAbiVersion

-- | Compatible-additions revision within the ABI version (LDICT_API_REVISION).
apiRevision :: IO Word32
apiRevision = cApiRevision

check :: Status -> IO ()
check 0 = pure ()
check _ = cLastError >>= peekCString >>= throwIO . userError

domainCode :: UnitDomain -> Word32
domainCode = fromIntegral . (+ 1) . fromEnum

withDictionary :: Dictionary -> (Ptr LdictDictionary -> IO a) -> IO a
withDictionary (Dictionary value) = withForeignPtr value

newDictionary :: (Ptr (Ptr LdictDictionary) -> IO Status) -> IO Dictionary
newDictionary constructor = alloca $ \output -> do
  constructor output >>= check
  Dictionary <$> (peek output >>= newForeignPtr cDictionaryFree)

dynamicDawg :: UnitDomain -> IO Dictionary
dynamicDawg domain = newDictionary (cDynamic (domainCode domain))

scdawg :: UnitDomain -> IO Dictionary
scdawg domain = newDictionary (cScdawg (domainCode domain))

withByteStrings :: [ByteString] -> ([Ptr Word8] -> [CSize] -> IO a) -> IO a
withByteStrings [] action = action [] []
withByteStrings (value : rest) action =
  BSU.unsafeUseAsCStringLen value $ \(pointer, length') ->
    withByteStrings rest $ \pointers lengths ->
      action (castPtr pointer : pointers) (fromIntegral length' : lengths)

optionalArrays :: [Maybe Word64] -> ([Word64], [Word8])
optionalArrays = unzip . fmap (maybe (0, 0) (\value -> (value, 1)))

doubleArrayTrie :: UnitDomain -> [(ByteString, Maybe Word64)] -> IO Dictionary
doubleArrayTrie domain entries =
  let (terms, options) = unzip entries
      (values, present) = optionalArrays options
  in withByteStrings terms $ \pointers lengths ->
       withArray pointers $ \dataPointer -> withArray lengths $ \lengthPointer ->
       withArray values $ \valuePointer -> withArray present $ \presentPointer ->
       newDictionary (cDat (domainCode domain) dataPointer lengthPointer
         valuePointer presentPointer (fromIntegral (length entries)))

withPath :: FilePath -> (Ptr Word8 -> CSize -> IO a) -> IO a
withPath path action = BSU.unsafeUseAsCStringLen
  (TextEncoding.encodeUtf8 (Text.pack path)) $ \(p, n) ->
  action (castPtr p) (fromIntegral n)

persistent :: (Ptr Word8 -> CSize -> Ptr (Ptr LdictDictionary) -> IO Status)
           -> FilePath -> IO Dictionary
persistent constructor path = withPath path $ \pointer length' ->
  newDictionary (constructor pointer length')

createPersistentARTrie, openPersistentARTrie :: UnitDomain -> FilePath -> IO Dictionary
createPersistentARTrie domain = persistent (cPersistentCreate (domainCode domain))
openPersistentARTrie domain = persistent (cPersistentOpen (domainCode domain))
createPersistentVocabulary, openPersistentVocabulary :: FilePath -> IO Dictionary
createPersistentVocabulary = persistent cVocabCreate
openPersistentVocabulary = persistent cVocabOpen

close :: Dictionary -> IO ()
close (Dictionary value) = finalizeForeignPtr value

resource :: Dictionary -> IO DictionaryResource
resource dictionary = withDictionary dictionary $ \pointer -> alloca $ \status -> do
  owned <- cResource pointer status
  peek status >>= check
  if owned == nullPtr then throwIO (userError "resource allocation failed")
                      else fromOwnedResource owned

scalarOutput :: Storable a => Dictionary -> a
             -> (Ptr LdictDictionary -> Ptr a -> IO Status) -> IO a
scalarOutput dictionary initial action = withDictionary dictionary $ \pointer ->
  alloca $ \output -> poke output initial >> action pointer output >>= check >> peek output

dictionaryLength :: Dictionary -> IO Int
dictionaryLength dictionary = fromIntegral <$> scalarOutput dictionary 0 cLength
dictionaryKind :: Dictionary -> IO Word32
dictionaryKind dictionary = scalarOutput dictionary 0 cKind
capabilities :: Dictionary -> IO Word64
capabilities dictionary = scalarOutput dictionary 0 cCapabilities

withBytes :: ByteString -> (Ptr Word8 -> CSize -> IO a) -> IO a
withBytes value action = BSU.unsafeUseAsCStringLen value $ \(p, n) ->
  action (castPtr p) (fromIntegral n)

putBytes :: Dictionary -> ByteString -> Maybe Word64 -> IO Bool
putBytes dictionary text mapped = withDictionary dictionary $ \d -> withBytes text $ \p n ->
  alloca $ \inserted -> do
    let (value, present) = maybe (0, 0) (\item -> (item, 1)) mapped
    cPutText d p n value present inserted >>= check
    (/= 0) <$> peek inserted
putText :: Dictionary -> Text -> Maybe Word64 -> IO Bool
putText dictionary = putBytes dictionary . TextEncoding.encodeUtf8

putManyBytes :: Dictionary -> [(ByteString, Maybe Word64)] -> IO Int
putManyBytes dictionary entries =
  let (terms, options) = unzip entries; (values, present) = optionalArrays options in
  withDictionary dictionary $ \d -> withByteStrings terms $ \pointers lengths ->
  withArray pointers $ \ps -> withArray lengths $ \ls -> withArray values $ \vs ->
  withArray present $ \hs -> alloca $ \inserted -> do
    cPutBatch d ps ls vs hs (fromIntegral (length entries)) inserted >>= check
    fromIntegral <$> peek inserted

boolText :: (Ptr LdictDictionary -> Ptr Word8 -> CSize -> Ptr Word8 -> IO Status)
         -> Dictionary -> ByteString -> IO Bool
boolText action dictionary text = withDictionary dictionary $ \d -> withBytes text $ \p n ->
  alloca $ \output -> action d p n output >>= check >> ((/= 0) <$> peek output)
removeBytes, containsBytes :: Dictionary -> ByteString -> IO Bool
removeBytes = boolText cRemoveText
containsBytes = boolText cContainsText
removeText, containsText :: Dictionary -> Text -> IO Bool
removeText dictionary = removeBytes dictionary . TextEncoding.encodeUtf8
containsText dictionary = containsBytes dictionary . TextEncoding.encodeUtf8

getBytes :: Dictionary -> ByteString -> IO Lookup
getBytes dictionary text = withDictionary dictionary $ \d -> withBytes text $ \p n ->
  alloca $ \isFound -> alloca $ \value -> alloca $ \hasValue -> do
    cGetText d p n isFound value hasValue >>= check
    found' <- (/= 0) <$> peek isFound
    present <- (/= 0) <$> peek hasValue
    mapped <- if present then Just <$> peek value else pure Nothing
    pure (Lookup found' mapped)
getText :: Dictionary -> Text -> IO Lookup
getText dictionary = getBytes dictionary . TextEncoding.encodeUtf8

withTokens :: [Word64] -> (Ptr Word64 -> CSize -> IO a) -> IO a
withTokens values action = withArray values $ \pointer ->
  action pointer (fromIntegral (length values))
putU64 :: Dictionary -> [Word64] -> Maybe Word64 -> IO Bool
putU64 dictionary tokens mapped = withDictionary dictionary $ \d -> withTokens tokens $ \p n ->
  alloca $ \inserted -> do
    let (value, present) = maybe (0, 0) (\item -> (item, 1)) mapped
    cPutU64 d p n value present inserted >>= check
    (/= 0) <$> peek inserted
boolU64 :: (Ptr LdictDictionary -> Ptr Word64 -> CSize -> Ptr Word8 -> IO Status)
        -> Dictionary -> [Word64] -> IO Bool
boolU64 action dictionary tokens = withDictionary dictionary $ \d -> withTokens tokens $ \p n ->
  alloca $ \output -> action d p n output >>= check >> ((/= 0) <$> peek output)
removeU64, containsU64 :: Dictionary -> [Word64] -> IO Bool
removeU64 = boolU64 cRemoveU64
containsU64 = boolU64 cContainsU64
getU64 :: Dictionary -> [Word64] -> IO Lookup
getU64 dictionary tokens = withDictionary dictionary $ \d -> withTokens tokens $ \p n ->
  alloca $ \isFound -> alloca $ \value -> alloca $ \hasValue -> do
    cGetU64 d p n isFound value hasValue >>= check
    found' <- (/= 0) <$> peek isFound; present <- (/= 0) <$> peek hasValue
    mapped <- if present then Just <$> peek value else pure Nothing
    pure (Lookup found' mapped)

clear :: Dictionary -> IO ()
clear dictionary = withDictionary dictionary (cClear >=> check)
compact :: Dictionary -> IO Int
compact dictionary = fromIntegral <$> scalarOutput dictionary 0 cCompact

operationCode :: AlgebraOperation -> Word32
operationCode Union = 1
operationCode Intersection = 2
operationCode Difference = 3
operationCode SymmetricDifference = 4

valueMergeCode :: ValueMerge -> Word32
valueMergeCode First = 1
valueMergeCode Last = 2
valueMergeCode LatticeJoin = 3
valueMergeCode LatticeMeet = 4

-- | Materialize an independent mutable DynamicDAWG with one native ordered merge.
algebra :: AlgebraOperation -> ValueMerge -> Dictionary -> Dictionary -> IO Dictionary
algebra operation valueMerge left right =
  withDictionary left $ \leftPointer -> withDictionary right $ \rightPointer ->
    newDictionary (cAlgebra leftPointer rightPointer
      (operationCode operation) (valueMergeCode valueMerge))

-- | Keys present in either input, using right-biased shared values.
union :: Dictionary -> Dictionary -> IO Dictionary
union = algebra Union Last

-- | Shared keys, using the optional-u64 lattice meet.
intersection :: Dictionary -> Dictionary -> IO Dictionary
intersection = algebra Intersection LatticeMeet

-- | Keys present in the left input but absent from the right.
difference :: Dictionary -> Dictionary -> IO Dictionary
difference = algebra Difference First

-- | Keys present in exactly one input.
symmetricDifference :: Dictionary -> Dictionary -> IO Dictionary
symmetricDifference = algebra SymmetricDifference First
checkpoint :: Dictionary -> IO ()
checkpoint dictionary = withDictionary dictionary (cCheckpoint >=> check)
containsSubstring :: Dictionary -> ByteString -> IO Bool
containsSubstring = boolText cSubstring
substringFrequency :: Dictionary -> ByteString -> IO Int
substringFrequency dictionary text = withDictionary dictionary $ \d -> withBytes text $ \p n ->
  alloca $ \output -> cFrequency d p n output >>= check >> (fromIntegral <$> peek output)

vocabularyTerm :: Dictionary -> Word64 -> IO (Maybe ByteString)
vocabularyTerm dictionary index = withDictionary dictionary $ \d ->
  alloca $ \lengthPointer -> alloca $ \isFound -> do
    cVocabTerm d index nullPtr 0 lengthPointer isFound >>= check
    present <- (/= 0) <$> peek isFound
    if not present then pure Nothing else do
      length' <- fromIntegral <$> peek lengthPointer
      text <- allocaBytes length' $ \buffer -> do
        cVocabTerm d index buffer (fromIntegral length') lengthPointer isFound >>= check
        BS.packCStringLen (castPtr buffer, length')
      pure (Just text)

entryFailure :: Ptr LdictEntryCursor -> Status -> IO a
entryFailure cursor status = do
  local <- if cursor == nullPtr then pure nullPtr else cEntriesError cursor
  native <- cLastError
  localMessage <- if local == nullPtr then pure "" else peekCString local
  nativeMessage <- if native == nullPtr then pure "" else peekCString native
  let message = if null localMessage then nativeMessage else localMessage
  throwIO (userError (if null message
    then "dictionary entries failed with status " ++ show status
    else message))

readEntryMetadata :: Ptr LdictEntryCursor -> IO EntriesMetadata
readEntryMetadata cursor =
  alloca $ \unit -> alloca $ \valueDomain ->
  alloca $ \hasExact -> alloca $ \exact ->
  alloca $ \hasIdentity -> alloca $ \producer -> alloca $ \revision -> do
    cEntriesInfo cursor unit valueDomain hasExact exact hasIdentity producer revision
    unitCode <- peek unit
    domain' <- case unitCode of
      1 -> pure Byte
      2 -> pure UnicodeScalar
      3 -> pure U64
      _ -> throwIO (userError "unknown dictionary entries unit domain")
    valueCode <- peek valueDomain
    optional <- case valueCode of
      0 -> pure False
      1 -> pure True
      _ -> throwIO (userError "unknown dictionary entries value domain")
    exact' <- (/= 0) <$> peek hasExact
    exactValue <- if exact' then Just . fromIntegral <$> peek exact else pure Nothing
    identity <- (/= 0) <$> peek hasIdentity
    identityValue <- if identity
      then Just <$> ((,) <$> peek producer <*> peek revision)
      else pure Nothing
    pure EntriesMetadata
      { entriesUnitDomain = domain'
      , entriesHaveOptionalValues = optional
      , entriesExactLength = exactValue
      , entriesSnapshotIdentity = identityValue
      }

acquireEntryStream :: EntryBatchLimits -> Dictionary -> IO EntryStream
acquireEntryStream limits dictionary
  | maxBatchEntries limits <= 0 || maxBatchUnits limits < 0 || maxBatchValues limits < 0 =
      ioError (userError "invalid dictionary entry batch limits")
  | otherwise = withDictionary dictionary $ \raw -> alloca $ \statusPointer -> do
      cursor <- cEntriesOpen raw
        (fromIntegral (maxBatchEntries limits))
        (fromIntegral (maxBatchUnits limits))
        (fromIntegral (maxBatchValues limits)) statusPointer
      status <- peek statusPointer
      if status /= 0 || cursor == nullPtr
        then entryFailure cursor status
        else (EntryStream <$> readEntryMetadata cursor <*> pure cursor)
          `onException` cEntriesFree cursor

releaseEntryStream :: EntryStream -> IO ()
releaseEntryStream stream = do
  status <- cEntriesClose (entryCursor stream)
  if status == 0
    then cEntriesFree (entryCursor stream)
    else do
      local <- cEntriesError (entryCursor stream)
      message <- if local == nullPtr then pure "" else peekCString local
      cEntriesFree (entryCursor stream)
      throwIO (userError (if null message then "closing dictionary entries failed" else message))

-- | Bracket a one-shot pull stream. Native batches are copied and released
-- before 'nextEntry' returns, and the cursor closes on normal return or exception.
withEntryStream :: Dictionary -> (EntryStream -> IO a) -> IO a
withEntryStream = withEntryStreamLimits defaultEntryBatchLimits

withEntryStreamLimits :: EntryBatchLimits -> Dictionary -> (EntryStream -> IO a) -> IO a
withEntryStreamLimits limits dictionary = bracket
  (acquireEntryStream limits dictionary) releaseEntryStream

nextEntry :: EntryStream -> IO (Maybe DictionaryEntry)
nextEntry stream =
  alloca $ \units -> alloca $ \lengthPointer -> alloca $ \value ->
  alloca $ \hasValue -> alloca $ \hasEntry -> do
    status <- cEntriesNext (entryCursor stream) units lengthPointer value hasValue hasEntry
    if status == 1 then pure Nothing else if status /= 0
      then entryFailure (entryCursor stream) status
      else do
        present <- (/= 0) <$> peek hasEntry
        if not present then pure Nothing else do
          pointer <- peek units
          count <- fromIntegral <$> peek lengthPointer
          key <- case entriesUnitDomain (entryMetadata stream) of
            Byte | count == 0 -> pure (ByteKey BS.empty)
            Byte -> ByteKey <$> BS.packCStringLen (castPtr pointer, count)
            UnicodeScalar -> do
              scalars <- peekArray count (castPtr pointer :: Ptr Word32)
              pure (UnicodeKey (Text.pack (map (chr . fromIntegral) scalars)))
            U64 -> U64Key <$> peekArray count (castPtr pointer :: Ptr Word64)
          mapped <- (/= 0) <$> peek hasValue
          mappedValue' <- if mapped then Just <$> peek value else pure Nothing
          pure (Just (DictionaryEntry key mappedValue'))

foldEntries :: Dictionary -> (a -> DictionaryEntry -> IO a) -> a -> IO a
foldEntries = foldEntriesWithLimits defaultEntryBatchLimits

foldEntriesWithLimits :: EntryBatchLimits -> Dictionary -> (a -> DictionaryEntry -> IO a) -> a -> IO a
foldEntriesWithLimits limits dictionary action initial = withEntryStreamLimits limits dictionary $ \stream ->
  let loop accumulator = nextEntry stream >>= maybe (pure accumulator)
        (action accumulator >=> loop)
  in loop initial

materializeEntries :: Dictionary -> IO (DictionarySnapshot DictionaryEntry)
materializeEntries dictionary = withEntryStream dictionary $ \stream -> do
  let loop reversed = nextEntry stream >>= maybe
        (pure (DictionarySnapshot (entryMetadata stream) (reverse reversed)))
        (\entry -> loop (entry : reversed))
  loop []

import {
  pathMap, suffixIndex, UnsupportedBackendError,
  type LibdictensteinNamespace,
} from "../index.js";

type IsNever<T> = [T] extends [never] ? true : false;
type Assert<T extends true> = T;

type PathMapRejects = Assert<IsNever<ReturnType<typeof pathMap>>>;
type SuffixIndexRejects = Assert<IsNever<ReturnType<typeof suffixIndex>>>;
type NamespacePathMapRejects = Assert<IsNever<ReturnType<LibdictensteinNamespace["pathMap"]>>>;
type NamespaceSuffixRejects = Assert<IsNever<ReturnType<LibdictensteinNamespace["suffixIndex"]>>>;

declare const error: UnsupportedBackendError;
error.status satisfies 6;
error.requiredRevision satisfies 8;
error.apiRevision satisfies number | null;
error.backend satisfies string;

export type Rev8GateContract = [
  PathMapRejects, SuffixIndexRejects, NamespacePathMapRejects, NamespaceSuffixRejects,
];

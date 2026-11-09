/** A file type's tint, drawn from the semantic palette. */
export type FileTone = "accent" | "danger" | "success" | "info" | "warning";

/**
 * One row of a mock directory tree. The two members are a discriminated union
 * so a folder has no file type to render and a file always has one.
 */
export type FileTreeEntry =
  | { name: string; depth: number; folder: true }
  | { name: string; depth: number; folder?: false; type: string; tone?: FileTone };

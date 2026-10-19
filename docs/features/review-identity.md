# Raw review identity

`termal-review-identity-v1` is a host-owned profile with `schemaVersion: 1`.
It identifies the index, HEAD, every tracked worktree file, nonignored
untracked bytes, and initialized nested repositories in distinct layers.
The one-shot CLI, session API and `termal_review_identity` MCP operation use
the same engine. Activating the shared review procedure and obtaining live
reviewer witnesses are separate steps.

Index and HEAD records carry path, Git mode, object ID and object format;
index records also carry stage and flags. Local objects must exist. Worktree
regular files carry their raw SHA-256 digest and size; symlinks carry their
target without traversal. The profile records Unix permission bits or an
explicit Windows mode field without executable bits. Platform profiles
cannot substitute for each other. Junctions and mount points are unsupported.

Untracked discovery is a bounded host-owned breadth-first walk. Git never
enumerates untracked directories. One pinned
`check-ignore -z --stdin --non-matching -v --no-index` batch per level uses
bare candidate paths to classify ignore status before any descent or reparse
classification. Git lstat's each candidate for directory-pattern matching and
never enumerates.
Queries use a literal `./` prefix and exact reply-to-name mapping so a root
filename cannot be interpreted as Git pathspec magic.
Ignored junctions are pruned along with other ignored entries. Nonignored
junctions are refused before descent; directory symlinks are target-only.
Ignore inputs are repository `.gitignore` files and `info/exclude`; global
and system exclusions are pinned out. Policies of walked directories remain
identity inputs. Tracked paths remain inputs even beneath ignored trees,
where a link ancestor is refused.

The engine checks directory identity after enumeration, file identity around
raw reads, and performs two complete captures under one 40-second deadline.
This establishes stability observations, not an atomic filesystem snapshot.
Limits are 200,000 inventory records, 200,000 visited directory entries,
128 MiB per payload, 2 GiB of raw reads per capture, eight nesting levels,
64 repository nodes, 8 MiB of encoded inventory, and pages of at most
100 entries or 64 KiB. Unsupported, changing or over-budget input produces
a named refusal and no usable fallback fingerprint.

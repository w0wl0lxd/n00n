Persist deeply compacted session transcripts as bounded records so long-running sessions keep saving and resume normally.
Recovered sessions whose log was interrupted mid-compaction instead of refusing to open them.
Bounded transcript compaction nesting so a corrupt session log cannot overflow the stack on load.
Held transcript compaction nesting under the loader's cap when compacting, so a long-lived session is not flattened and rewritten on every reload.

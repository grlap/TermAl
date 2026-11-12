# Conversation Overview Map — Superseded

The colored conversation minimap and its `/overview` endpoint have been
removed. The original brief is retained in Git history.

Sessions use the native scrollbar for thumb, track, wheel and keyboard
navigation. Scrolling into unloaded history requests a bounded page through
the existing history loader. Checkpoint dots beside the scrollbar jump through
the existing marker navigation path; nearby checkpoints share a count and a
chooser with an entry for each checkpoint.

See [virtualized transcripts](session-virtualized-transcript.md#native-scrollbar-and-checkpoint-dots)
for the current navigation contract and [conversation markers](conversation-markers.md)
for the durable marker model.

The compact SQLite overview fields remain maintained by existing writers.
Navigation no longer reads them. This change removes no persisted column or
table and changes no persistence floor; storage cleanup is separate work.

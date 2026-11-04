-- Playing on another device (AGENTS.md "Playing on a TV"): a device whose
-- owner shares it takes videos from everyone in the house, not only from
-- their own devices. Presence and commands live in memory (remote.rs).
ALTER TABLE api_tokens ADD COLUMN shared boolean NOT NULL DEFAULT false;

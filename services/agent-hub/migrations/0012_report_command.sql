-- Problem reports name the command or operation that failed (RT-1163). Reports stored before
-- this migration, and from agents that do not send it, have an empty command.
ALTER TABLE reports ADD COLUMN command TEXT NOT NULL DEFAULT '';

-- Remote attach (RT-0261): a command may carry a secret (a product's one-time enrol code) for
-- the agent. It is kept apart from `args` so no view or log ever shows it, handed to the agent
-- once with the command and cleared in the same delivery (`takePending`).
ALTER TABLE commands ADD COLUMN secret TEXT;

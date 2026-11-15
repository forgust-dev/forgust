-- When an LFS object stopped being used by any project (NULL: in use, or not
-- looked at yet). The maintenance sweep marks the unused ones, and deletes
-- those still unused a day later, with their file. Linking an object to a
-- project again clears the mark.
ALTER TABLE lfs_objects ADD COLUMN unused_since timestamptz;
CREATE INDEX lfs_objects_unused_since_idx ON lfs_objects (unused_since) WHERE unused_since IS NOT NULL;

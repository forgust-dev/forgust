-- Imports from Gitea and Forgejo, besides GitLab and GitHub.
ALTER TABLE imports DROP CONSTRAINT imports_source_check;
ALTER TABLE imports ADD CONSTRAINT imports_source_check CHECK (source IN ('gitlab', 'github', 'gitea', 'forgejo'));

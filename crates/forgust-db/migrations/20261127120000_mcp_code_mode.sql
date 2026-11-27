-- The MCP server's code mode (the search_api and execute_api tools): scripts
-- an assistant writes, run in a sandbox on the server. Off until an
-- administrator turns it on.
ALTER TABLE instance_settings ADD COLUMN mcp_code_mode boolean NOT NULL DEFAULT false;

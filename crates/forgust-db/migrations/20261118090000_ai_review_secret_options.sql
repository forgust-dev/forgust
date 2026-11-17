-- The reviewer's options that are secrets, set in the admin area: a JSON map,
-- encrypted like the key.
ALTER TABLE ai_review_instance ADD COLUMN secret_options_enc text;

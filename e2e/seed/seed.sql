-- Seed data for end-to-end tests. NOW_MS is replaced with the current time.
-- People and their colors (0 and 1 in oxbot's color list).
INSERT INTO people (name, color, first_ms) VALUES ('Jay', 0, NOW_MS - 900000000), ('Sam', 1, NOW_MS - 800000000);

-- A shared bot with a short chat.
INSERT INTO events (bot, seq, ts_ms, body) VALUES
  ('recipes', 1, NOW_MS - 7200000, '{"kind":"user_message","text":"Something quick with lentils?","from":"Jay"}'),
  ('recipes', 2, NOW_MS - 7190000, '{"kind":"assistant_message","text":"Red lentil dal: 25 minutes, one pot. Want the steps?"}'),
  ('recipes', 3, NOW_MS - 7189000, '{"kind":"turn_done","ok":true,"duration_ms":4200}');

-- Sam's private bot. Its words must never reach anyone else's screen.
INSERT INTO events (bot, seq, ts_ms, body) VALUES
  ('bank', 1, NOW_MS - 3600000, '{"kind":"user_message","text":"What is my balance?","from":"Sam"}'),
  ('bank', 2, NOW_MS - 3590000, '{"kind":"assistant_message","text":"Your balance is $4,210 SECRET-BALANCE."}'),
  ('bank', 3, NOW_MS - 3589000, '{"kind":"turn_done","ok":true,"duration_ms":3100}');

-- Signed-in sites: one at home (shared bots), one in Sam's own browser.
INSERT INTO sites (owner, site, signed_in_ms) VALUES ('', 'wikipedia.org', NOW_MS - 86400000), ('Sam', 'bank.example', NOW_MS - 86400000);

-- Claude usage so Settings shows numbers.
INSERT INTO usage (bot, turns, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost_usd) VALUES
  ('recipes', 1, 1200, 340, 9000, 2000, 0.02);

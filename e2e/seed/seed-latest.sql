-- Seed rows for tables newer than seed.sql's schema, added after oxbot has
-- upgraded the database. NOW_MS is replaced with the current time.

-- A research report in Sam's private bot. It must never reach anyone else.
INSERT INTO reports (id, bot, seq, created_ms, asked_by, question, title, summary, markdown, sources, links) VALUES
  ('r_5ec2e75ec2e75ec2e75ec2e75ec2e75e', 'bank', 2, NOW_MS - 3590000, NULL, 'Compare savings rates',
   'SECRET-REPORT', 'SECRET-REPORT summary', 'Rates are best at SECRET-REPORT bank [1].',
   '[{"n":1,"url":"https://bank.example.com/rates","title":"Rates","site":"example.com","read_ms":0}]', '[]');

-- Jay's Journal is shared with Sam (picked people): Sam uses it, Ann can't.
INSERT INTO bot_members (bot, person, added_ms) SELECT 'journal', id, NOW_MS FROM people WHERE name = 'Sam';

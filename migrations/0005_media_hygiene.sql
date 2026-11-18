-- nujijushi 0005: per-media sensitivity, and no source hashtag by default

-- Several sources flag sensitivity per item rather than per post (twitter marks
-- individual images), and a post whose only flag sat on one item used to be
-- archived as sfw.
ALTER TABLE media ADD COLUMN sensitive INTEGER NOT NULL DEFAULT 0;

-- "#twitter" / "#pixiv" at the end of every caption was noise. New channels
-- default to off (model::Channel::new); this flips the existing ones so the
-- behaviour matches what was asked for. Set include_source_tag = 1 per channel
-- to bring it back.
UPDATE channels SET include_source_tag = 0;

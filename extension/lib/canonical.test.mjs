/* Table tests for the canonicalizer.
 *
 *   node --test extension/lib/canonical.test.mjs
 *
 * No dependencies and no browser: lib/canonical.js exports itself to
 * module.exports for exactly this. Add a case here whenever you add a site —
 * this file is the contract the content-script adapters and the panel rely on.
 */
import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { canonicalize, splitUrls } = require("./canonical.js");

const cases = [
  // --- X / Twitter: every URL form of one post collapses to the same key ---
  ["https://x.com/Artist/status/123", "x:123", "https://x.com/Artist/status/123", "x", null],
  ["https://twitter.com/Artist/status/123?s=20&t=abcdef", "x:123", "https://x.com/Artist/status/123", "x", null],
  ["https://mobile.twitter.com/Artist/status/123/photo/1", "x:123", "https://x.com/Artist/status/123", "x", null],
  ["https://www.x.com/Artist/status/123/video/1", "x:123", "https://x.com/Artist/status/123", "x", null],
  ["https://x.com/Artist/statuses/123", "x:123", "https://x.com/Artist/status/123", "x", null],
  ["https://x.com/i/web/status/123", "x:123", "https://x.com/i/status/123", "x", null],
  ["https://fxtwitter.com/Artist/status/123", "x:123", "https://x.com/Artist/status/123", "x", null],
  ["https://vxtwitter.com/Artist/status/123", "x:123", "https://x.com/Artist/status/123", "x", null],
  ["x.com/Artist/status/123", "x:123", "https://x.com/Artist/status/123", "x", null],
  ["  <https://x.com/Artist/status/123>  ", "x:123", "https://x.com/Artist/status/123", "x", null],

  // --- pixiv: the ?p= page hint is not part of the identity ---
  ["https://www.pixiv.net/artworks/456", "pixiv:456", "https://www.pixiv.net/artworks/456", "pixiv", null],
  ["https://www.pixiv.net/en/artworks/456?p=3", "pixiv:456", "https://www.pixiv.net/artworks/456", "pixiv", 3],
  ["https://www.pixiv.net/member_illust.php?illust_id=456", "pixiv:456", "https://www.pixiv.net/artworks/456", "pixiv", null],
  ["https://www.pixiv.net/i/456", "pixiv:456", "https://www.pixiv.net/artworks/456", "pixiv", null],

  // --- RedNote: one note identity, with the original route and share values ---
  [
    "https://www.rednote.com/discovery/item/674051740000000007027A15?xsec_token=fixture%2Bvalue=&utm_source=demo&xsec_source=pc_feed#here",
    "rednote:674051740000000007027a15",
    "https://www.rednote.com/discovery/item/674051740000000007027A15?xsec_token=fixture%2Bvalue%3D&xsec_source=pc_feed",
    "rednote",
    null,
  ],
  [
    "https://www.xiaohongshu.com/user/profile/5c31698d0000000007018a31/674051740000000007027a15/",
    "rednote:674051740000000007027a15",
    "https://www.xiaohongshu.com/user/profile/5c31698d0000000007018a31/674051740000000007027a15",
    "rednote",
    null,
  ],
  [
    "https://rednote.com/en/discovery/item/674051740000000007027a15",
    "rednote:674051740000000007027a15",
    "https://rednote.com/en/discovery/item/674051740000000007027a15",
    "rednote",
    null,
  ],
  [
    "https://xhslink.cn/a/Fixture?xsec_source=app_share",
    "rednote-share:xhslink.cn/a/Fixture",
    "https://xhslink.cn/a/Fixture?xsec_source=app_share",
    "rednote",
    null,
  ],

  // --- boorus ---
  ["https://danbooru.donmai.us/posts/789?q=tag", "danbooru:789", "https://danbooru.donmai.us/posts/789", "danbooru", null],
  [
    "https://gelbooru.com/index.php?page=post&s=view&id=321",
    "gelbooru:321",
    "https://gelbooru.com/index.php?page=post&s=view&id=321",
    "gelbooru",
    null,
  ],

  // --- other sites ---
  ["https://www.artstation.com/artwork/abcdEF", "artstation:abcdEF", "https://www.artstation.com/artwork/abcdEF", "artstation", null],
  [
    "https://www.deviantart.com/someone/art/A-Title-123456",
    "deviantart:123456",
    "https://www.deviantart.com/someone/art/A-Title-123456",
    "deviantart",
    null,
  ],
  ["https://blog.tumblr.com/post/999/some-title", "tumblr:blog:999", "https://blog.tumblr.com/post/999", "tumblr", null],
  [
    "https://bsky.app/profile/artist.bsky.social/post/3kabc",
    "bluesky:artist.bsky.social:3kabc",
    "https://bsky.app/profile/artist.bsky.social/post/3kabc",
    "bluesky",
    null,
  ],
  ["https://mastodon.social/@artist/112233", "mastodon:mastodon.social:112233", "https://mastodon.social/@artist/112233", "mastodon", null],
  ["https://t.me/night_art/42", "telegram:night_art:42", "https://t.me/night_art/42", "telegram", null],
  ["https://t.me/s/night_art/42", "telegram:night_art:42", "https://t.me/night_art/42", "telegram", null],
  ["https://t.me/c/1234567/42", "telegram:c1234567:42", "https://t.me/c/1234567/42", "telegram", null],

  // --- unknown but http(s): accepted, kept as a generic identity ---
  ["https://Example.COM/a/b/?utm_source=x&b=2&a=1#frag", "url:example.com/a/b?a=1&b=2", "https://example.com/a/b?a=1&b=2", "web", null],
  ["https://x.com/Artist", "url:x.com/Artist", "https://x.com/Artist", "web", null],
  ["https://x.com/Artist/", "url:x.com/Artist", "https://x.com/Artist", "web", null],
  // the protocol is preserved for unknown sites (http-only hosts stay reachable)
  ["http://example.com/x", "url:example.com/x", "http://example.com/x", "web", null],
];

const rejects = [
  "",
  "   ",
  "hello world",
  "/Users/me/Pictures/dump",
  "~/Pictures",
  "./relative/path",
  "C:\\Users\\me\\Pictures",
  "javascript:alert(1)",
  "data:text/plain,hi",
  "file:///Users/me/a.png",
  "chrome://bookmarks",
  "mailto:someone@example.com",
  "not-a-url",
  "https://www.rednote.com/explore",
  "https://www.rednote.com/explore/not-a-note",
  "https://www.xiaohongshu.com/user/profile/5c31698d0000000007018a31",
  "https://name@www.rednote.com/explore/674051740000000007027a15",
  "https://xhslink.com/bad/path",
];

test("canonical URLs, keys and sources", () => {
  for (const [input, key, url, source, page] of cases) {
    const got = canonicalize(input);
    assert.ok(got, `expected a link for ${input}`);
    assert.equal(got.key, key, `key for ${input}`);
    assert.equal(got.url, url, `url for ${input}`);
    assert.equal(got.source, source, `source for ${input}`);
    assert.equal(got.page, page, `page for ${input}`);
  }
});

test("the same post pasted in different shapes has one key", () => {
  const shapes = [
    "https://x.com/Artist/status/123",
    "https://twitter.com/Artist/status/123?s=20",
    "https://mobile.twitter.com/Artist/status/123/photo/1",
    "https://x.com/i/web/status/123",
    "https://fxtwitter.com/Artist/status/123",
    "x.com/Artist/status/123",
  ];
  const keys = new Set(shapes.map((shape) => canonicalize(shape).key));
  assert.equal(keys.size, 1, `got ${[...keys].join(", ")}`);
});

test("non-links are rejected rather than turned into junk identities", () => {
  for (const input of rejects) {
    assert.equal(canonicalize(input), null, `expected null for ${input}`);
  }
});

test("percent-encoded newlines split like real newlines", () => {
  const blob =
    "https://x.com/a/status/1%0Ahttps://x.com/a/status/2%0Ahttps://www.pixiv.net/artworks/3%0D%0A";
  const keys = splitUrls(blob).map((url) => canonicalize(url)?.key);
  assert.deepEqual(keys, ["x:1", "x:2", "pixiv:3"]);
  // a genuine plus sign in a URL must survive
  assert.deepEqual(splitUrls("https://ex.com/a+b%0Ahttps://ex.com/c"), ["https://ex.com/a+b", "https://ex.com/c"]);
});

test("a link that still holds an encoded newline is refused, not stored", () => {
  assert.equal(canonicalize("https://x.com/a/status/1%0Ahttps://x.com/a/status/2"), null);
});

import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
require("./canonical.js");
const { serverAddress, pagePost, watchCandidate } = require("./admin.js");

test("server addresses preserve base paths and permit plain HTTP on loopback", () => {
  assert.deepEqual(serverAddress("https://example.test:8443/archive/"), {
    base: "https://example.test:8443/archive",
    permission: "https://example.test/*",
  });
  assert.equal(serverAddress("http://127.0.0.1:8788").base, "http://127.0.0.1:8788");
  assert.deepEqual(serverAddress("  archive.example.org:8443/admin/  "), {
    base: "https://archive.example.org:8443/admin",
    permission: "https://archive.example.org/*",
  });
  for (const url of ["http://example.test", "https://name@host.test", "https://host.test/?token=bad"]) {
    assert.equal(serverAddress(url), null);
  }
});

test("direct imports accept a post on the matching supported site only", () => {
  const page = "https://www.rednote.com/explore";
  const direct = page + "/674051740000000007027a15?xsec_token=sample%2Bvalue%3D&xsec_source=pc_feed";
  assert.equal(pagePost(direct, page).url, direct);
  assert.equal(pagePost(direct, "https://x.com/home"), null);
  assert.equal(pagePost("https://example.test/elsewhere", page), null);
  assert.equal(pagePost(page, page), null);
  assert.equal(pagePost("https://www.fanbox.cc/@artist/posts/5", "https://artist.fanbox.cc").url,
    "https://artist.fanbox.cc/posts/5");
});

test("watch candidates derive the artist and check handle-less X posts by status ID", () => {
  const x = "https://x.com/i/web/status/123";
  assert.equal(watchCandidate(x, "https://x.com/quoted/status/999"), null);
  assert.deepEqual(watchCandidate(x, "https://x.com/artist/status/123"), {
    url: "https://x.com/artist/media", name: "@artist",
  });
  assert.equal(watchCandidate("https://twitter.com/artist").url, "https://x.com/artist/media");
  assert.equal(watchCandidate("https://www.pixiv.net/en/artworks/123").url, "https://www.pixiv.net/artworks/123");
  assert.equal(watchCandidate("https://www.pixiv.net/en/users/456").url, "https://www.pixiv.net/users/456/artworks");
  assert.equal(watchCandidate("https://artist.fanbox.cc/posts/456").url, "https://artist.fanbox.cc/posts");
  assert.equal(watchCandidate("https://bsky.app/profile/artist.example/post/abc").url, "https://bsky.app/profile/artist.example/media");
  assert.equal(watchCandidate("https://www.rednote.com/user/profile/5c31698d0000000007018a31"), null);
});

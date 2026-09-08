"""Focused contracts for fork release validation; no network or mutations."""

import json
import unittest
from unittest.mock import patch

from scripts import fork_release as release


class ForkReleaseTests(unittest.TestCase):
    def test_strict_versions(self):
        release.validate_version("1.2.3+fork.1", "1.2.3")
        release.validate_version("1.2.3+fork.10", "1.2.3+fork.9")
        for version in ("1.2.3", "v1.2.3+fork.1", "1.2.3+fork.0", "1.2.3+fork.01",
                        "01.2.3+fork.1", "1.02.3+fork.1", "1.2.03+fork.1",
                        "1.2.3+fork.1\n", "1.2.3+fork.1;echo bad", "1.2.4+fork.1"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                release.validate_version(version, "1.2.3")

    def test_revision_and_exact_match(self):
        for version in ("1.2.3+fork.8", "1.2.3+fork.9"):
            with self.assertRaises(ValueError):
                release.validate_version(version, "1.2.3+fork.9")
        release.validate_version("1.2.3+fork.9", "1.2.3+fork.9", exact=True)
        with self.assertRaises(ValueError):
            release.validate_version("1.2.3+fork.10", "1.2.3+fork.9", exact=True)

    def test_remote_allowlist(self):
        for prefix in ("https://github.com/", "git@github.com:", "ssh://git@github.com/"):
            for suffix in ("", ".git"):
                release.validate_remote(prefix + release.REPOSITORY + suffix)
        for url in ("https://github.com/herdrdev/herdr.git", "https://evil.test/trungnt13/herdr",
                    "https://github.com/trungnt13/herdr/", "https://github.com/trungnt13/herdr.git?x",
                    "https://github.com@evil.test/trungnt13/herdr", "file:///trungnt13/herdr"):
            with self.subTest(url=url), self.assertRaises(ValueError):
                release.validate_remote(url)

    @patch.object(release, "read_command")
    def test_destination_requires_all_push_urls_and_identity(self, command):
        url = "git@github.com:trungnt13/herdr.git"
        repository = json.dumps({"full_name": release.REPOSITORY, "permissions": {"push": True}})
        command.side_effect = [url, url, "trungnt13", repository]
        release.validate_destination()
        for responses in ([url, url + "\ngit@github.com:herdrdev/herdr.git"],
                          [url, url, "someone-else"],
                          [url, url, "trungnt13", '{"full_name":"herdrdev/herdr","permissions":{"push":true}}'],
                          [url, url, "trungnt13", '{"full_name":"trungnt13/herdr","permissions":{"push":false}}']):
            command.side_effect = responses
            with self.assertRaises(ValueError):
                release.validate_destination()

    @patch.object(release, "read_command")
    def test_remote_tags_and_paginated_draft_reservations(self, command):
        for tags, pages in (("abc\trefs/tags/v1.2.3+fork.10", [[]]),
                            ("", [[], [{"tag_name": "v1.2.3+fork.10", "draft": True}]]),
                            ("abc\trefs/tags/v1.2.3+fork.9", [[]])):
            command.side_effect = [tags, "\n".join(item["tag_name"] for page in pages for item in page)]
            with self.assertRaises(ValueError):
                release.validate_reservations("1.2.3+fork.9")
        command.side_effect = ["abc\trefs/tags/v1.2.3+fork.9", "v2.0.0+fork.99"]
        release.validate_reservations("1.2.3+fork.10")
        command.assert_called_with("gh", "api", "--hostname", "github.com", "--paginate", "--jq", ".[].tag_name", "repos/trungnt13/herdr/releases?per_page=100")


if __name__ == "__main__":
    unittest.main()

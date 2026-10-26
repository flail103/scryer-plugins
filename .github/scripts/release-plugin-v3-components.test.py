#!/usr/bin/env python3
"""Exercise release trigger component selection against a synthetic repository."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parent / "release-plugin-v3-components.sh"

# Isolated synthetic repository only; gh is a shell mock.
MOCK = r'''set -euo pipefail
gh() {
  printf 'gh %s\n' "$*" >> calls
  if [[ "$1 $2" == "api repos/example/plugins/git/tags/"* ]]; then
    local object="${2##*/}" verified=true tagger=human@example.test
    if [[ " $UNVERIFIED " == *" $object "* ]]; then verified=false; fi
    if [[ " $BOT_TAGS " == *" $object "* ]]; then tagger=Bot@Example.Test; fi
    printf '%s\t%s\n' "$verified" "$tagger"
    return 0
  fi
  return 1
}
source "$SCRIPT"
release_plugin_components "$@"
'''


def git(repo, *args):
    return subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@example.test", "-c", "commit.gpgsign=false",
         "-c", "tag.gpgsign=false", *args],
        cwd=repo, check=True, capture_output=True, text=True, timeout=30,
    ).stdout.strip()


class ComponentTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="release-components-test-")
        self.repo = Path(self.directory.name)
        git(self.repo, "init", "-q", "-b", "main")
        (self.repo / "a").write_text("1")
        git(self.repo, "add", ".")
        git(self.repo, "commit", "-q", "-m", "legacy")
        self.legacy = git(self.repo, "rev-parse", "HEAD")
        (self.repo / "a").write_text("2")
        git(self.repo, "commit", "-q", "-am", "head")
        self.head = git(self.repo, "rev-parse", "HEAD")
        git(self.repo, "checkout", "-q", "-b", "side", self.legacy)
        (self.repo / "b").write_text("3")
        git(self.repo, "add", ".")
        git(self.repo, "commit", "-q", "-m", "side")
        self.side = git(self.repo, "rev-parse", "HEAD")
        git(self.repo, "checkout", "-q", "main")
        git(self.repo, "tag", "-a", "-m", "Release", "plugins-v3/newznab/v2.3.3", self.legacy)
        git(self.repo, "tag", "-a", "-m", "Release", "plugins-v3/newznab/v3.1.0", self.head)
        git(self.repo, "tag", "-a", "-m", "Release", "plugins-v3/torznab/v1.0.0", self.side)
        git(self.repo, "tag", "plugins-v3/jimaku/v1.0.0", self.legacy)
        git(self.repo, "tag", "-a", "-m", "Release", "rule-packs/trash/v1.0.0", self.head)

    def tearDown(self):
        self.directory.cleanup()

    def trigger(self, body, name="plugins-v3/release/1700000000-abcdef012345", marker=True, at=None):
        count = sum(1 for line in body.splitlines() if line.startswith("component "))
        message = "Release trigger for components\n\n"
        if marker:
            message += f"components {count}\n"
        message += body
        git(self.repo, "tag", "-a", "-m", message, name, at or self.head)
        return name

    def run_components(self, trigger, *, release_sha=None, verify="true", unverified="", bot_tags="",
                       bot_email="bot@example.test"):
        result = subprocess.run(
            ["bash", "-c", MOCK, "components", trigger, release_sha or self.head, "example/plugins", verify],
            cwd=self.repo, capture_output=True, text=True, timeout=30,
            env={**os.environ, "SCRIPT": str(SCRIPT), "UNVERIFIED": unverified, "BOT_TAGS": bot_tags,
                 "SCRYER_AUTOMATED_RELEASE_BOT_EMAIL": bot_email},
        )
        calls = (self.repo / "calls").read_text() if (self.repo / "calls").exists() else ""
        return result, calls

    def test_listed_components_build_from_their_own_commits(self):
        trigger = self.trigger(
            f"component plugins-v3/newznab/v3.1.0 {self.head}\n"
            f"component plugins-v3/newznab/v2.3.3 {self.legacy}\n"
        )
        result, calls = self.run_components(trigger)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout,
            f"newznab\t3.1.0\t{self.head}\nnewznab\t2.3.3\t{self.legacy}\n",
        )
        # The trigger tag and both component tags are verified.
        self.assertEqual(calls.count("gh api repos/example/plugins/git/tags/"), 3)

    def test_the_trigger_tag_itself_is_verified(self):
        trigger = self.trigger(f"component plugins-v3/newznab/v3.1.0 {self.head}\n")
        trigger_object = git(self.repo, "rev-parse", f"refs/tags/{trigger}")
        result, calls = self.run_components(trigger, unverified=trigger_object)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not a GitHub-verified signed tag", result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertEqual(calls.count("gh api"), 1)

    def test_a_trigger_declaring_zero_components_builds_nothing(self):
        # A rule-pack-only trigger at a commit that already carries plugin tags
        # must not rebuild and re-sign those plugins.
        trigger = self.trigger("")
        result, _ = self.run_components(trigger)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_a_trigger_without_a_components_marker_is_refused(self):
        trigger = self.trigger("", marker=False)
        result, _ = self.run_components(trigger, verify="false")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no components marker", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_a_lightweight_or_missing_trigger_is_refused(self):
        git(self.repo, "tag", "plugins-v3/release/light", self.head)
        for trigger in ["plugins-v3/release/light", "plugins-v3/release/absent"]:
            with self.subTest(trigger=trigger):
                result, _ = self.run_components(trigger, verify="false")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("not an annotated tag", result.stderr)
                self.assertEqual(result.stdout, "")

    def test_a_mismatched_component_count_is_refused(self):
        message = f"Release trigger\n\ncomponents 2\ncomponent plugins-v3/newznab/v3.1.0 {self.head}\n"
        git(self.repo, "tag", "-a", "-m", message, "plugins-v3/release/short", self.head)
        result, _ = self.run_components("plugins-v3/release/short", verify="false")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("declares 2 components but lists 1", result.stderr)

    def test_crlf_component_lines_are_accepted(self):
        message = f"Release trigger\r\n\r\ncomponents 1\r\ncomponent plugins-v3/newznab/v2.3.3 {self.legacy}\r\n"
        git(self.repo, "tag", "-a", "-m", message, "plugins-v3/release/crlf", self.head)
        result, _ = self.run_components("plugins-v3/release/crlf", verify="false")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f"newznab\t2.3.3\t{self.legacy}\n")

    def test_without_a_trigger_the_release_commit_tags_and_its_triggers_are_used(self):
        # Catalog-only and manual runs: tags at the commit plus the components
        # that triggers created there list at earlier commits, deduplicated.
        self.trigger(
            f"component plugins-v3/newznab/v3.1.0 {self.head}\n"
            f"component plugins-v3/newznab/v2.3.3 {self.legacy}\n"
        )
        self.trigger("", name="plugins-v3/release/catalog-1-from-x", marker=False)
        result, calls = self.run_components("", verify="false")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout,
            f"newznab\t3.1.0\t{self.head}\nnewznab\t2.3.3\t{self.legacy}\n",
        )
        self.assertEqual(calls, "")
        result, calls = self.run_components("", verify="true")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls.count("gh api"), 2)

    def test_without_a_trigger_only_the_release_commit_tags_are_used_when_no_trigger_lists_more(self):
        result, _ = self.run_components("", verify="false")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f"newznab\t3.1.0\t{self.head}\n")

    def test_a_tag_that_does_not_round_trip_is_refused(self):
        git(self.repo, "tag", "-a", "-m", "Release", "plugins-v3/a/b/v1.0.0", self.head)
        trigger = self.trigger(f"component plugins-v3/a/b/v1.0.0 {self.head}\n")
        result, _ = self.run_components(trigger, verify="false")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsafe id or version", result.stderr)

    def test_a_release_without_plugin_tags_prints_nothing(self):
        (self.repo / "c").write_text("4")
        git(self.repo, "add", ".")
        git(self.repo, "commit", "-q", "-m", "packs only")
        packs_only = git(self.repo, "rev-parse", "HEAD")
        git(self.repo, "tag", "-a", "-m", "Release", "rule-packs/trash/v1.1.0", packs_only)
        result, _ = self.run_components("", release_sha=packs_only)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_lightweight_component_tags_are_refused(self):
        trigger = self.trigger(f"component plugins-v3/jimaku/v1.0.0 {self.legacy}\n")
        result, _ = self.run_components(trigger)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not an annotated tag", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_a_component_listed_at_the_wrong_commit_is_refused(self):
        trigger = self.trigger(f"component plugins-v3/newznab/v2.3.3 {self.head}\n")
        result, _ = self.run_components(trigger)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("points at", result.stderr)

    def test_a_component_outside_the_release_history_is_refused(self):
        trigger = self.trigger(f"component plugins-v3/torznab/v1.0.0 {self.side}\n")
        result, _ = self.run_components(trigger)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not an ancestor", result.stderr)

    def test_an_unverified_component_tag_is_refused_when_verifying(self):
        trigger = self.trigger(f"component plugins-v3/newznab/v2.3.3 {self.legacy}\n")
        tag_object = git(self.repo, "rev-parse", "refs/tags/plugins-v3/newznab/v2.3.3")
        result, _ = self.run_components(trigger, unverified=tag_object)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not a GitHub-verified signed tag", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_malformed_and_duplicate_component_lines_are_refused(self):
        bodies = [
            f"component plugins-v3/newznab/v2.3.3 {self.legacy} extra\n",
            f"component plugins-v3/newznab/v2.3.3 {self.legacy}\ncomponent plugins-v3/newznab/v2.3.3 {self.legacy}\n",
            f"component rule-packs/trash/v1.0.0 {self.head}\n",
            "component plugins-v3/newznab/v2.3.3 abc\n",
        ]
        for index, body in enumerate(bodies):
            with self.subTest(body=body):
                trigger = self.trigger(body, name=f"plugins-v3/release/t{index}")
                result, _ = self.run_components(trigger)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")

    def object_of(self, tag):
        return git(self.repo, "rev-parse", f"refs/tags/{tag}")

    def cardigann_release(self, version, *, at=None):
        git(self.repo, "tag", "-a", "-m", "Release", f"plugins-v3/cardigann-engine/v{version}", at or self.head)
        return f"plugins-v3/cardigann-engine/v{version}"

    def bot_trigger(self, *components, name="plugins-v3/release/resume-bot"):
        body = "".join(f"component {tag} {self.head}\n" for tag in components)
        trigger = self.trigger(body, name=name)
        return trigger, " ".join(self.object_of(tag) for tag in (trigger, *components))

    def test_the_bot_may_release_a_cardigann_patch(self):
        self.cardigann_release("0.4.1", at=self.legacy)
        trigger, bot_tags = self.bot_trigger(self.cardigann_release("0.4.2"))
        result, _ = self.run_components(trigger, bot_tags=bot_tags)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f"cardigann-engine\t0.4.2\t{self.head}\n")

    def test_a_bot_signed_trigger_may_not_release_another_plugin(self):
        # Even when the component tag itself is human-signed.
        trigger = self.trigger(f"component plugins-v3/newznab/v3.1.0 {self.head}\n", name="plugins-v3/release/bot")
        result, _ = self.run_components(trigger, bot_tags=self.object_of(trigger))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("may not release newznab", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_a_bot_signed_component_may_not_be_another_plugin_under_a_human_trigger(self):
        trigger = self.trigger(f"component plugins-v3/newznab/v3.1.0 {self.head}\n")
        result, _ = self.run_components(trigger, bot_tags=self.object_of("plugins-v3/newznab/v3.1.0"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("may not release newznab", result.stderr)

    def test_the_bot_may_only_patch_an_existing_cardigann_line(self):
        self.cardigann_release("0.4.1", at=self.legacy)
        for index, version in enumerate(["0.5.0", "1.0.0", "0.4.1-rc.1"]):
            with self.subTest(version=version):
                tag = self.cardigann_release(version)
                trigger, bot_tags = self.bot_trigger(tag, name=f"plugins-v3/release/bot-{index}")
                result, _ = self.run_components(trigger, bot_tags=bot_tags)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")
                git(self.repo, "tag", "-d", tag, trigger)

    def test_the_bot_may_not_release_a_first_cardigann_version(self):
        trigger, bot_tags = self.bot_trigger(self.cardigann_release("0.4.2"), name="plugins-v3/release/first")
        result, _ = self.run_components(trigger, bot_tags=bot_tags)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("patch bump on an already released 0.4 line", result.stderr)

    def test_an_automated_patch_stays_valid_after_later_releases(self):
        # Catalog-only republication re-verifies older component tags.
        self.cardigann_release("0.4.1", at=self.legacy)
        trigger, bot_tags = self.bot_trigger(self.cardigann_release("0.4.2"))
        self.cardigann_release("0.5.0", at=self.legacy)
        result, _ = self.run_components(trigger, bot_tags=bot_tags)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f"cardigann-engine\t0.4.2\t{self.head}\n")

    def test_a_bot_signed_rule_pack_trigger_still_lists_nothing(self):
        trigger = self.trigger("", name="plugins-v3/release/bot-packs")
        result, _ = self.run_components(trigger, bot_tags=self.object_of(trigger))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_human_releases_are_unaffected_by_the_bot_policy(self):
        trigger = self.trigger(f"component plugins-v3/newznab/v3.1.0 {self.head}\n")
        for bot_email in ["bot@example.test", ""]:
            with self.subTest(bot_email=bot_email):
                result, _ = self.run_components(trigger, bot_email=bot_email)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, f"newznab\t3.1.0\t{self.head}\n")


if __name__ == "__main__":
    unittest.main()

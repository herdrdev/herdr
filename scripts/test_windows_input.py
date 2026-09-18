"""Portable tests of the gauntlet's oracle, not Windows input qualification."""
import copy
import unittest
from scripts.windows_input.report import catalogue, channel_identity_errors, herdr_protocol_label, known_host_gap, qualification_matrix, summarize, verdict


class WindowsInputGauntletTests(unittest.TestCase):
    def setUp(self):
        self.matrix = catalogue()
        self.cases = {case["id"]: case for case in self.matrix["cases"]}
        self.evidence = dict(ready=True, focus_verified=True, complete=True, width=121, height=24,
                             outer_geometry=[121, 24, 0x298], pane_geometry=[100, 20, 0x200],
                             final_outer_geometry=[121, 24, 0x298], final_pane_geometry=[100, 20, 0x200])

    def test_catalogue_has_unique_cases_and_geometry_boundaries(self):
        self.assertEqual(len(self.cases), len(self.matrix["cases"]))
        self.assertEqual(self.matrix["widths"], [80, 119, 120, 121, 132, 160, 240])
        self.assertEqual(self.matrix["heights"], [24, 50])
        for case in self.cases.values():
            self.assertTrue(case["id"])
            self.assertLessEqual(set(case["expected"]), set(self.matrix["modes"]))
            for expected in case["expected"].values():
                for value in expected.get("hex", []):
                    self.assertTrue(bytes.fromhex(value))
        self.assertEqual(self.cases["dead-acute"]["kind"], "layout-key")
        self.assertEqual(self.cases["altgr-euro"]["kind"], "manual")

    def test_default_catalogue_does_not_inject_terminal_host_actions(self):
        for case_id in ["ctrl-v", "alt-enter", "ctrl-shift-up", "ctrl-shift-down", "ctrl-shift-home", "ctrl-shift-end"]:
            self.assertEqual(self.cases[case_id]["kind"], "qualification", case_id)

    def test_kitty_function_keys_accept_native_and_legacy_encodings(self):
        self.assertEqual(self.cases["shift-tab"]["expected"]["kitty"]["hex"], ["1b5b5a", "1b5b393b3275"])
        self.assertEqual(self.cases["up"]["expected"]["kitty"]["hex"], ["1b5b41", "1b5b353734313975"])

    def test_shift_enter_cannot_pass_with_plain_enter_or_trailing_duplicates(self):
        case = self.cases["shift-enter"]
        correct = "1b5b32373b323b31337e"
        for value, status in [("0d", "fail"), (correct, "pass"), (correct + "0d", "fail")]:
            self.assertEqual(verdict(case, "mok2", {**self.evidence, "hex": value})[0], status)
        self.assertEqual(verdict(case, "legacy", {**self.evidence, "hex": "0d"})[0], "unsupported")

    def test_focus_readiness_and_actual_geometry_are_required(self):
        case = self.cases["letter-a"]
        good = {**self.evidence, "hex": "61"}
        for field in ["ready", "focus_verified", "complete", "pane_geometry", "outer_geometry", "final_pane_geometry", "final_outer_geometry"]:
            value = copy.deepcopy(good)
            value.pop(field)
            self.assertEqual(verdict(case, "legacy", value)[0], "inconclusive", field)
        self.assertEqual(verdict(case, "legacy", {**good, "outer_geometry": [120, 24]})[0], "inconclusive")
        for malformed in [1, "121,24", {}, [121], [121, "24"], [True, 24]]:
            self.assertEqual(verdict(case, "legacy", {**good, "pane_geometry": malformed})[0], "inconclusive")

    def test_native_modifiers_release_and_repeat_are_not_discarded(self):
        # type, down, repeat, vk, scan, Unicode, control-state
        self.evidence["scans"] = [42, 28]
        records = [[1, 1, 1, 13, 28, 13, 16], [1, 0, 1, 13, 28, 13, 16]]
        case = self.cases["shift-enter"]
        self.assertEqual(verdict(case, "native", {**self.evidence, "records": records})[0], "pass")
        for index, value in [(6, 0), (2, 2), (3, 10), (4, 0), (5, 122)]:
            bad = copy.deepcopy(records)
            bad[0][index] = value
            self.assertEqual(verdict(case, "native", {**self.evidence, "records": bad})[0], "fail")
        self.assertEqual(verdict(case, "native", {**self.evidence, "records": records[:1]})[0], "fail")
        extra = [[1, 1, 1, 16, 42, 0, 16]] + records
        self.assertEqual(verdict(case, "native", {**self.evidence, "records": extra})[0], "fail")
        balanced = extra + [[1, 0, 1, 16, 42, 0, 0]]
        self.assertEqual(verdict(case, "native", {**self.evidence, "records": balanced})[0], "pass")

    def test_herdr_native_page_keys_must_be_consumed_before_the_pane(self):
        case = self.cases["page-up"]
        evidence = {**self.evidence, "path": "herdr", "scans": [73]}
        records = [[1, 1, 1, 33, 73, 0, 0], [1, 0, 1, 33, 73, 0, 0]]
        self.assertEqual(verdict(case, "native", {**evidence, "records": []})[0], "inconclusive")
        self.assertEqual(verdict(case, "native", {**evidence, "records": [[4, 0, 0, 0, 0, 0, 0]]})[0], "inconclusive")
        self.assertEqual(verdict(case, "native", {**evidence, "records": records})[0], "fail")
        self.assertEqual(verdict(case, "native", {**evidence, "records": [None]})[0], "inconclusive")
        self.assertEqual(verdict(case, "native", {**evidence, "path": "direct", "records": records})[0], "pass")
        for mode in ("legacy", "mok2", "kitty"):
            self.assertEqual(verdict(case, mode, {**evidence, "hex": ""})[0], "inconclusive")
            self.assertEqual(verdict(case, mode, {**evidence, "hex": "1b5b357e"})[0], "fail")

    def test_paste_requires_one_envelope_and_complete_unicode_payload(self):
        case = self.cases["paste-unicode"]
        payload = case["text"].replace("\n", "\r\n").encode()
        framed = b"\x1b[200~" + payload + b"\x1b[201~"
        for data, expected in [(framed, "pass"), (payload, "fail"), (framed * 2, "fail"),
                               (framed[:-1], "fail"), (framed + b"\r", "fail"),
                               (b"\x1b[200~\xff\x1b[201~", "fail")]:
            self.assertEqual(verdict(case, "kitty", {**self.evidence, "hex": data.hex()})[0], expected)

    def test_remote_clipboard_image_requires_empty_host_paste_and_staged_png_path(self):
        case = self.cases["clipboard-image"]
        empty_paste = b"\x1b[200~\x1b[201~"
        staged = b"\x1b[200~C:\\Temp\\herdr-clipboard-images-user\\image.png\x1b[201~"
        self.assertEqual(verdict(case, "legacy", {**self.evidence, "path": "direct", "hex": empty_paste.hex()})[0], "pass")
        remote = {**self.evidence, "path": "herdr-remote", "hex": staged.hex(),
                  "staged_image_sha256": case["expected"]["legacy"]["sha256"]}
        self.assertEqual(verdict(case, "legacy", remote)[0], "pass")
        self.assertEqual(verdict(case, "legacy", {**remote, "staged_image_sha256": "0" * 64})[0], "fail")
        self.assertEqual(verdict(case, "legacy", {**self.evidence, "path": "herdr-remote", "hex": empty_paste.hex()})[0], "fail")
        self.assertEqual(verdict(case, "legacy", {**self.evidence, "path": "herdr", "hex": empty_paste.hex()})[0], "not_run")

    def test_mouse_interleave_requires_ordered_motion_and_paste(self):
        case = self.cases["mouse-interleave"]
        motion = b"\x1b[<35;10;5M"
        paste = b"\x1b[200~mouse\r\npaste\x1b[201~"
        good = b"a" + motion + paste + motion + b"b"
        self.assertEqual(verdict(case, "kitty", {**self.evidence, "hex": good.hex()})[0], "pass")
        for bad in (good.replace(motion, b"", 1), good + b"b", b"a" + motion + motion + paste + b"b"):
            self.assertEqual(verdict(case, "kitty", {**self.evidence, "hex": bad.hex()})[0], "fail")

    def test_runtime_mode_transition_has_one_exact_order(self):
        case = self.cases["mode-transitions"]
        for mode in ("legacy", "mok2", "kitty"):
            expected = case["expected"][mode]["hex"][0]
            self.assertEqual(verdict(case, mode, {**self.evidence, "hex": expected})[0], "pass")
            self.assertEqual(verdict(case, mode, {**self.evidence, "hex": expected + "0d"})[0], "fail")
        direct = {**self.evidence, "path": "direct"}
        for observed in ("a\rb\rc\rd\re\rf", "a\rb\rc\x1b[13;2ud\re\rf"):
            self.assertEqual(verdict(case, "legacy", {**direct, "hex": observed.encode().hex()})[0], "unsupported")

    def test_mouse_focus_refresh_requires_reports_on_both_sides(self):
        case = self.cases["mouse-focus-refresh"]
        mouse = b"\x1b[<35;10;5M\x1b[<0;10;5M\x1b[<0;10;5m\x1b[<64;10;5M"
        good = b"a" + mouse + b"bc" + mouse + b"d"
        self.assertEqual(verdict(case, "legacy", {**self.evidence, "hex": good.hex()})[0], "pass")
        for bad in (b"ab" + b"c" + mouse + b"d", b"a" + mouse + b"bcd"):
            self.assertEqual(verdict(case, "legacy", {**self.evidence, "hex": bad.hex()})[0], "fail")

    def test_qualification_matrix_uses_observed_results_only(self):
        observations = [
            {"host": "stable", "case": "shift-enter", "path": "herdr", "mode": "mok2", "status": "pass"},
            {"host": "stable", "case": "shift-enter", "path": "direct", "mode": "legacy", "status": "unsupported"},
            {"host": "stable", "case": "shift-enter", "path": "direct", "mode": "kitty", "status": "pass"},
            {"host": "stable", "case": "shift-enter", "path": "direct", "mode": "kitty", "status": "inconclusive"},
        ]
        rows = {row[0]: row[1:] for row in qualification_matrix({"channels": ["stable"], "observations": observations})}
        self.assertEqual(rows["Shift+Enter"], ("PASS", "X - becomes Enter", "PASS**"))
        self.assertEqual(rows["Multiline paste"], ("NOT TESTED", "NOT TESTED", "NOT TESTED"))
        observations.append({"host": "stable", "case": "dead-acute", "path": "herdr", "mode": "mok2", "status": "pass"})
        observations.extend([
            {"host": "stable", "case": "letter-a", "path": "direct", "mode": "legacy", "width": 80, "status": "pass"},
            {"host": "stable", "case": "shift-enter", "path": "direct", "mode": "legacy", "width": 80, "status": "unsupported"},
            {"host": "stable", "case": "paste-lf", "path": "direct", "mode": "legacy", "width": 80, "status": "pass"},
            {"host": "stable", "case": "mouse-focus-refresh", "path": "herdr", "mode": "legacy", "width": 80, "status": "pass"},
            {"host": "stable", "case": "mouse-focus-refresh", "path": "direct", "mode": "legacy", "width": 80, "status": "pass"},
            {"host": "stable", "case": "mouse-focus-refresh", "path": "direct", "mode": "kitty", "width": 80, "status": "pass"},
        ])
        rows = {row[0]: row[1:] for row in qualification_matrix({"channels": ["stable"], "observations": observations})}
        self.assertEqual(rows["Dead-key composition"][0], "PASS")
        self.assertEqual(rows["Resize 120 -> 80"][1], "PASS")
        self.assertEqual(rows["Mouse after resize"], ("PASS", "PASS", "PASS"))
        self.assertEqual(rows["AltGr"], ("MANUAL", "MANUAL", "MANUAL"))
        self.assertEqual(rows["IME composition"], ("MANUAL", "MANUAL", "MANUAL"))

    def test_qualification_matrix_requires_every_selected_channel(self):
        observations = [
            {"host": "stable", "case": "shift-enter", "path": "herdr", "mode": "mok2", "status": "pass"},
        ]
        for result in ({"channels": ["stable", "preview"], "observations": observations},
                       {"observations": observations}):
            rows = {row[0]: row[1:] for row in qualification_matrix(result)}
            self.assertEqual(rows["Shift+Enter"][0], "PARTIAL")

    def test_herdr_protocol_label_requires_every_run_to_prove_transport(self):
        result = {"hosts": [{"channel": "stable", "runs": [{"nonce": "one", "path": "herdr", "mode": "native"},
                                                                  {"nonce": "two", "path": "herdr", "mode": "native"}]}],
                  "observations": [{"host": "stable", "nonce": "one", "path": "herdr", "mode": "native",
                                    "input_reader": "windows-console",
                                    "input_transport": "win32-serialized"}]}
        self.assertEqual(herdr_protocol_label(result), "Herdr default (UNKNOWN)*")
        result["observations"].append({"host": "stable", "nonce": "two", "path": "herdr", "mode": "native",
                                       "input_reader": "windows-console",
                                       "input_transport": "win32-serialized"})
        self.assertEqual(herdr_protocol_label(result), "Win32 (Herdr)*")
        result["hosts"].append({"channel": "preview", "runs": [{"nonce": "one", "path": "herdr", "mode": "native"}]})
        self.assertEqual(herdr_protocol_label(result), "Herdr default (UNKNOWN)*")

    def test_empty_partial_and_duplicate_reports_never_become_green(self):
        self.assertFalse(summarize({})["observed_checks_passed"])
        row = {**self.evidence, "case": "letter-a", "host": "stable", "path": "herdr", "mode": "legacy", "hex": "61", "phase": 1,
               "width": 120, "height": 30, "outer_geometry": [120, 30], "final_outer_geometry": [120, 30]}
        self.assertFalse(summarize({"observations": [row]})["observed_checks_passed"])
        with self.assertRaisesRegex(ValueError, "Duplicate"):
            summarize({"observations": [row, row]})
        later = {**row, "phase": 2, "width": 80, "height": 24, "outer_geometry": [80, 24], "final_outer_geometry": [80, 24]}
        host = {"channel": "stable", "runs": [{"nonce": "owned", "path": "herdr", "mode": "legacy", "pid": 123, "hwnd": 456,
                                                "elevated": False, "image_identity": "image-s", "installation_identity": "install-s"}]}
        row.update(nonce="owned", capture_id="first")
        later.update(nonce="owned", capture_id="second")
        self.assertEqual(summarize({"observations": [row, later], "hosts": [host], "controller_elevated": False})["counts"]["pass"], 2)
        stale = {**later, "capture_id": "first"}
        self.assertEqual(summarize({"observations": [row, stale], "hosts": [host], "controller_elevated": False})["counts"]["inconclusive"], 1)
        forged_hosts = [{"channel": name, "runs": [{}]} for name in ("stable", "preview")]
        partial = summarize({"observations": [row], "hosts": forged_hosts})
        self.assertFalse(partial["observed_checks_passed"])
        self.assertGreater(partial["coverage_missing"], 0)
        for status in ["unsupported", "not_run", "inconclusive"]:
            self.assertEqual(verdict(self.cases["letter-a"], "legacy", {**row, "status": status})[0], status)

    def test_malformed_native_records_are_inconclusive_not_exceptions(self):
        evidence = {**self.evidence, "scans": [30]}
        for records in [None, 7, "records", [None], [[1, 1, 1, 65, 30, 97, None]],
                        [[True, 1, 1, 65, 30, 97, 0]], [[1, 1, 1, 65, 30, 97, "0"]]]:
            self.assertEqual(verdict(self.cases["letter-a"], "native", {**evidence, "records": records})[0], "inconclusive")

    def test_known_host_gap_only_exempts_observed_terminal_versions_and_cases(self):
        row = {**self.evidence, "case": "shift-enter", "host": "stable", "path": "direct", "mode": "mok2", "hex": "0d", "phase": 1,
               "width": 120, "height": 30, "outer_geometry": [120, 30], "final_outer_geometry": [120, 30],
               "nonce": "owned", "capture_id": "fresh"}
        for path, version, raw, expected in [("direct", "1.24.11911.0", "0d", "unsupported"),
                                             ("direct", "1.25.1912.0", "0d", "unsupported"),
                                             ("direct", "1.26.1.0", "0d", "fail"),
                                             ("herdr", "1.24.11911.0", "0d", "fail"),
                                             ("direct", "1.24.11911.0", "", "fail")]:
            observation = {**row, "path": path, "hex": raw}
            run = {"nonce": "owned", "path": path, "mode": "mok2", "pid": 123, "hwnd": 456, "terminal_version": version,
                   "elevated": False, "image_identity": "image-s", "installation_identity": "install-s"}
            document = {"observations": [observation], "hosts": [{"channel": "stable", "runs": [run]}], "controller_elevated": False}
            result = summarize(document)["observations"][0]
            self.assertEqual(result["status"], expected)
            self.assertEqual(result["failure_scope"], "direct_host" if path == "direct" else "through_herdr_not_yet_attributed")
            document["controller_elevated"] = True
            self.assertEqual(summarize(document)["observations"][0]["status"], "inconclusive")
            document["controller_elevated"] = False
            run["elevated"] = True
            self.assertEqual(summarize(document)["observations"][0]["status"], "inconclusive")

    def test_direct_mok_legacy_fallback_is_case_and_version_bounded(self):
        case = self.cases["shift-tab"]
        direct = {**self.evidence, "path": "direct", "mode": "mok2", "hex": "1b5b5a"}
        self.assertTrue(known_host_gap(direct, case, "1.25.2607.10002"))
        self.assertFalse(known_host_gap(direct, case, "1.26.0.0"))
        self.assertFalse(known_host_gap({**direct, "path": "herdr"}, case, "1.25.2607.10002"))
        self.assertFalse(known_host_gap({**direct, "hex": "1b5b32373b323b397e"}, case, "1.25.2607.10002"))

    def test_duplicate_channel_identity_is_rejected_at_both_stages(self):
        hosts = [{"channel": name, "launcher_identity": name + "-exe", "installation_identity": name + "-dir",
                  "runs": [{"image_identity": name + "-image", "installation_identity": name + "-dir", "process_identity": name + "-pid/start"}]}
                 for name in ("stable", "preview")]
        self.assertEqual(channel_identity_errors(hosts), [])
        for field in ("launcher_identity", "installation_identity"):
            duplicate = copy.deepcopy(hosts)
            duplicate[1][field] = duplicate[0][field]
            self.assertTrue(channel_identity_errors(duplicate))
            self.assertTrue(summarize({"hosts": duplicate})["errors"])
        for field in ("image_identity", "installation_identity", "process_identity"):
            duplicate = copy.deepcopy(hosts)
            duplicate[1]["runs"][0][field] = duplicate[0]["runs"][0][field]
            self.assertTrue(channel_identity_errors(duplicate))


if __name__ == "__main__":
    unittest.main()

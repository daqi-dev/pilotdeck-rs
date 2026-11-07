# anchor-selfcheck: unsupported meta-test of the shared copy manifest, builds throwaway git repos
"""#1570: the scratch copy must carry every compile-time input.

`source_copy()` copies `src/` plus whatever `compile_inputs()` derives from
the sources. A missed input does not fail as a surviving mutant: the copy does
not compile, `run()` records INVALID, and the control silently stops
discriminating while the summary reads like an environment problem.

These cases pin the derivation and its fail-closed edges. Each one builds a
throwaway git repository that mimics the workspace layout and runs the real
function against it -- no cargo, no network, no writes outside `tempfile`.

Run from `src/pilotdeck-gateway/tests/fixtures`:
``python3 -B -m unittest -v _shared.test_compile_inputs``.
"""
from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import unittest

from _shared import negative_control as control

SPEC = {'specs/x.json': '{}'}
EXTERNAL = (('specs',), ('specs/x.json',))


class Manifest(unittest.TestCase):
    def setUp(self):
        self._saved = (control.REPO, control.SRC)
        self.roots = []

    def tearDown(self):
        control.REPO, control.SRC = self._saved
        for root in self.roots:
            shutil.rmtree(root, ignore_errors=True)

    def workspace(self, source, extra=None, link=None):
        root = Path(tempfile.mkdtemp(prefix='pdrs-1570-'))
        self.roots.append(root)
        files = dict(SPEC, **{'LICENSE': 'x\n', 'rust-toolchain.toml': '[toolchain]\n'})
        files.update(extra or {})
        files['src/a.rs'] = source
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding='utf-8')
        if link:
            name, target = link
            os.symlink(target, root / name)
        subprocess.check_call(['git', 'init', '-q'], cwd=root)
        subprocess.check_call(['git', 'add', '-A'], cwd=root)
        subprocess.check_call(['git', '-c', 'user.email=t@t', '-c', 'user.name=t',
                               'commit', '-qm', 'x'], cwd=root)
        control.REPO, control.SRC = root, root / 'src'
        return root

    def derived(self, source, extra=None, link=None):
        self.workspace(source, extra, link)
        directories, files = control.compile_inputs()
        return (tuple(map(str, directories)), tuple(map(str, files)))

    def refused(self, source, extra=None, link=None):
        self.workspace(source, extra, link)
        with self.assertRaises(RuntimeError, msg='#1570 must refuse, not copy nothing'):
            control.compile_inputs()

    def test_literal_shapes_reach_the_same_input(self):
        for shape in ('include_str!("../specs/x.json")',
                      'include_str ! ( "../specs/x.json" )',
                      'include_str /* why */ ! ( "../specs/x.json" )',
                      'include_str! /* why */ ( "../specs/x.json" )',
                      'include_str // why\n    !("../specs/x.json")',
                      'include_str /* a /* nested */ b */ !("../specs/x.json")',
                      'include_str\x0b!("../specs/x.json")',
                      'include_str\x0c!("../specs/x.json")',
                      'include_str\u200e!("../specs/x.json")',
                      'include_str!\x0b("../specs/x.json")',
                      'include_str\u200f! ( "../specs/x.json" )',
                      'include_str\x85!("../specs/x.json")',
                      'include_str\u2028!("../specs/x.json")',
                      'include_str!["../specs/x.json"]',
                      'include_bytes! { "../specs/x.json" }',
                      'include_str!(r"../specs/x.json")',
                      'include_str!(r#"../specs/x.json"#)',
                      'include_str!("../specs/x.json",)',
                      'include_str!(\n        "../specs/x.json",\n    )',
                      'include_str!("../specs/x.json");\r'):
            with self.subTest(shape=shape):
                self.assertEqual(self.derived('const X: &str = ' + shape + ';'), EXTERNAL,
                                 f'#1570 missed a legal literal shape: {shape}')

    def test_non_rust_whitespace_is_not_trivia(self):
        # Rust's token separator is `Pattern_White_Space`, exactly the eleven
        # code points above. `str.isspace()` is wider in both directions:
        # `U+00A0`, `U+1680`, `U+2000`-`U+200A`, `U+202F`, `U+205F`, `U+3000`
        # and `U+001C`-`U+001F` are not Rust whitespace (`rustc` says "unknown
        # start of token: \u{a0}"), so a file holding one of them next to the
        # macro name does not compile and no file is read. Reading it as trivia
        # would invent a call; this is the other direction of the same bug.
        for shape in ('include_str\xa0!("../specs/x.json")',
                      'include_str\u1680!("../specs/x.json")',
                      'include_str\u2000!("../specs/x.json")',
                      'include_str\u3000!("../specs/x.json")',
                      'include_str\u202f!("../specs/x.json")',
                      'include_str\x1c!("../specs/x.json")',
                      'include_str\x1f!("../specs/x.json")'):
            with self.subTest(shape=shape):
                self.assertEqual(self.derived('const X: &str = ' + shape + ';'), ((), ()),
                                 f'#1570: {shape!r} is not a Rust call')

    def test_mismatched_delimiter_is_refused(self):
        self.refused('const X: &str = include_str!("../specs/x.json"];')

    def test_non_literal_argument_is_refused(self):
        self.refused('const X: &str = include_str!(PATH);')

    def test_missing_target_is_refused(self):
        self.refused('const X: &str = include_str!("../specs/gone.json");')

    def test_target_outside_the_repository_is_refused(self):
        self.refused('const X: &str = include_str!("../../outside.json");')

    def test_symlink_out_of_the_repository_is_refused(self):
        # `include!` and `#[path]` are optional only while their target is
        # absent: an existing file that resolves outside the checkout is a
        # real compile input the scratch copy cannot carry.
        for source, name in (('const X: &str = include_str!("../specs/link.json");', 'link.json'),
                             ('include!("../specs/link.rs");', 'link.rs'),
                             ('#[path = "../specs/link.rs"]\nmod m;', 'link.rs')):
            with self.subTest(source=source):
                self.refused(source, link=('specs/' + name, '/etc/hosts'))

    def test_untracked_src_target_is_carried(self):
        # `src/` gets the same carry-along rule as the derived directories
        # (#1570 r18), so a target the copy would carry is not refused here.
        # Refusing was the old contract, back when only `git ls-files src` was
        # copied; keeping it would stop every control in a tree with a pending
        # data file the copy already carries (#1570 r19 codex P1).
        root = self.workspace('const X: &str = include_str!("data.json");')
        (root / 'src/data.json').write_text('{}', encoding='utf-8')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-untracked-src-target-'))
        self.roots.append(destination)
        control.source_copy(destination)
        self.assertEqual((destination / 'src/data.json').read_text(encoding='utf-8'), '{}')

    def test_include_edge_carries_a_fragment_and_its_includes(self):
        self.assertEqual(
            self.derived('include!("gen/data.inc");',
                         {'src/gen/data.inc': 'const Y: &str = include_str!("../../specs/x.json");'}),
            EXTERNAL, '#1570 must follow include! into a non-.rs fragment')

    def test_path_attribute_carries_a_fragment_and_its_includes(self):
        self.assertEqual(
            self.derived('#[path = "gen/data.inc"]\nmod m;',
                         {'src/gen/data.inc': 'const Y: &str = include_str!("../../specs/x.json");'}),
            EXTERNAL, '#1570 must follow #[path] into a non-.rs fragment')

    def test_path_attribute_shapes_reach_the_same_input(self):
        for shape in ('#[path = "../specs/mod.rs"]',
                      '#![path = "../specs/mod.rs"]',
                      '#[r#path = "../specs/mod.rs"]',
                      '#[cfg_attr(all(), path = "../specs/mod.rs")]',
                      '#[path\n    = "../specs/mod.rs"]',
                      '#[path /* why */ = "../specs/mod.rs"]',
                      '#[path\x0b= "../specs/mod.rs"]',
                      '#[path =\u200e "../specs/mod.rs"]',
                      '#[cfg_attr(feature = r#"foo"]"#, path = "../specs/mod.rs")]',
                      '#[cfg_attr(feature = r##"a"#b"##, path = "../specs/mod.rs")]',
                      '#[cfg_attr(all(), doc = "first\nsecond", path = "../specs/mod.rs")]',
                      "#[cfg_attr(all(), doc = stringify!('a), path = \"../specs/mod.rs\")]",
                      "#[cfg_attr(all(), doc = stringify!('\\u{3b1}'), path = \"../specs/mod.rs\")]",
                      "#[cfg_attr(all(), doc = stringify!('\u03b1'), path = \"../specs/mod.rs\")]",
                      "#[cfg_attr(all(), doc = stringify!('\u03b1), path = \"../specs/mod.rs\")]",
                      # Unassigned in every Unicode version: the local tables
                      # cannot call it an identifier start, and stopping there
                      # would drop the path (codex r7).
                      "#[cfg_attr(all(), doc = stringify!('\u0378), path = \"../specs/mod.rs\")]",
                      # Added in Unicode 15.0: Python 3.9 does not know it.
                      "#[cfg_attr(all(), doc = stringify!('\U00011f02), path = \"../specs/mod.rs\")]",
                      '#[cfg_attr(all(), doc = stringify!(&\'static str), path = "../specs/mod.rs")]',
                      '#[cfg_attr(\n    all(),\n    path = "../specs/mod.rs",\n)]'):
            with self.subTest(shape=shape):
                self.assertEqual(
                    self.derived(shape + '\nmod m;', {'specs/mod.rs': 'pub const Z: u8 = 1;\n'}),
                    (('specs',), ('specs/mod.rs',)),
                    f'#1570 missed a legal #[path] shape: {shape}')

    def test_include_and_path_targets_outside_src_are_copied(self):
        self.assertEqual(self.derived('include!("../specs/frag.rs");',
                                      {'specs/frag.rs': 'pub const Z: u8 = 1;\n'}),
                         (('specs',), ('specs/frag.rs',)))
        self.assertEqual(self.derived('#[path = "../specs/mod.rs"]\nmod m;',
                                      {'specs/mod.rs': 'pub const Z: u8 = 1;\n'}),
                         (('specs',), ('specs/mod.rs',)))

    def test_absent_include_and_path_targets_are_fixture_text(self):
        # `unsafe_boundary.rs` writes `include!("../../outside.rs")` into a
        # string on purpose; refusing those would stop a control that works.
        # Written as raw strings so the macro text is unescaped in the fixture
        # source, which is the branch under test: matched, target absent.
        self.assertEqual(self.derived('fn f() { let _ = r#"include!("../../nope.rs");"#; }'),
                         ((), ()))
        self.assertEqual(self.derived('fn f() { let _ = r#"#[path = "../../nope.rs"]"#; }'),
                         ((), ()))

    def test_unresolvable_include_argument_is_refused(self):
        # `include!` expands its argument at compile time, so a splice this
        # scan cannot resolve may read a file the copy does not carry.
        for shape in ('include!(concat!("../specs/", "frag.rs"));',
                      'include!(env!("OUT_DIR"));',
                      'include!(PATH);'):
            with self.subTest(shape=shape):
                self.refused(shape, {'specs/frag.rs': 'pub const Z: u8 = 1;\n'})

    def test_comment_inside_a_real_include_argument_is_not_text(self):
        # `\` and a backtick are not Rust tokens, but they are legal *inside a
        # comment*, and the comment sits inside a real call. Skipping comments
        # is what keeps `include!(concat!(/* ` */ "../specs/", "frag.rs"))`
        # from passing for fixture text while it really splices a file.
        for shape in ('include!(concat!(/* ` */ "../specs/", "frag.rs"));',
                      'include!(concat!(/* \\ */ "../specs/", "frag.rs"));',
                      'include!(PATH /* ` */);'):
            with self.subTest(shape=shape):
                self.refused(shape, {'specs/frag.rs': 'pub const Z: u8 = 1;\n'})

    def test_char_literal_inside_a_real_include_argument_is_code(self):
        # `concat!` splices a char literal into the path, so the argument is
        # code, not fixture text. A scan that reads the quote inside the char
        # literal as an opening quote calls the whole argument "text", skips
        # the edge, and the copy misses a file the control really compiles.
        extra = {'specs/frag.rs': 'pub const Z: u8 = 1;\n'}
        for shape in ('include!(concat!("../specs/", \'"\', "frag.rs"));',
                      'include!(concat!("../specs/", \'`\', "frag.rs"));',
                      "include!(concat!(\"../specs/\", '\\\\', \"frag.rs\"));",
                      'include!(concat!(r#"../specs/"#, "frag.rs"));'):
            with self.subTest(shape=shape):
                self.refused(shape, extra)

    def test_escaped_path_is_refused(self):
        # `include!("../specs/\x66rag.rs")` reads `frag.rs`: an escape hides the
        # real name from a textual scan. Refuse instead of looking for a file
        # literally named `\x66rag.rs`, which would copy nothing.
        extra = {'specs/f.json': '{}', 'specs/mod.rs': 'pub const Z: u8 = 1;\n',
                 'specs/frag.rs': 'pub const Z: u8 = 1;\n'}
        for source in ('const X: &str = include_str!("../specs/\\x66.json");',
                       '#[path = "../specs/\\x6dod.rs"]\nmod m;',
                       'include!("../specs/\\x66rag.rs");'):
            with self.subTest(source=source):
                self.refused(source, extra)

    def test_include_text_that_cannot_be_code_is_not_refused(self):
        # #1570: the scan is textual, so fixture strings spell whole calls.
        # An argument that cannot be a string literal -- escaped quotes,
        # a raw-string prefix cut by an escape, a backtick, a block -- is
        # text, and refusing it would stop every control that runs today.
        for source in ('fn f() { let _ = "include!(\\"../../outside.rs\\");"; }',
                       'fn f() { let _ = "include!(concat!(\\"..\\", \\"/x.rs\\"));"; }',
                       'fn f() { let _ = "include!(r#\\"raw/x.inc\\"#);"; }',
                       'fn f() { let _ = "include!({literal});"; }',
                       '// a marker like `include/*c*/!(` appears once comments are gone\nfn f() {}',
                       'fn f() { let _ = format!("include!({name});"); }'):
            with self.subTest(source=source):
                self.assertEqual(self.derived(source), ((), ()))

    def test_untracked_include_and_path_targets_are_carried(self):
        # An untracked plain file under `src/` is part of the manifest now
        # (the copy carries untracked `src/` files), so derivation follows it
        # instead of refusing. It is a fragment, not data: whatever it holds
        # becomes an input too.
        for source, extra in (('include!("gen/data.inc");', 'src/gen/data.inc'),
                              ('#[path = "gen/data.inc"]\nmod m;', 'src/gen/data.inc')):
            with self.subTest(source=source):
                root = self.workspace(
                    source, {extra: 'pub const Z: &str = include_str!("../../specs/x.json");\n'})
                subprocess.check_call(['git', 'rm', '-q', '--cached', extra], cwd=root)
                self.assertEqual(
                    (tuple(map(str, control.compile_inputs()[0])),
                     tuple(map(str, control.compile_inputs()[1]))), EXTERNAL)
                destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-untracked-inc-'))
                self.roots.append(destination)
                control.source_copy(destination)
                self.assertEqual((destination / extra).read_text(encoding='utf-8'),
                                 'pub const Z: &str = include_str!("../../specs/x.json");\n')
                self.assertEqual(
                    (destination / 'specs/x.json').read_text(encoding='utf-8'), '{}')

    def test_root_level_input_is_a_file_not_a_directory(self):
        self.assertEqual(
            self.derived('const X: &str = include_str!("../VERSION");', {'VERSION': '1\n'}),
            ((), ('VERSION',)),
            '#1570: a root-level input has no directory worth copying; it must '
            'come back as a file so callers can still copy it')

    def test_source_copy_lands_the_derived_inputs(self):
        # The derivation is only half of #1570: `source_copy` must actually
        # put those files in the copy. Asserting the manifest alone would pass
        # while the copy stayed broken.
        root = self.workspace('const A: &str = include_str!("../specs/x.json");\n'
                              'const B: &str = include_str!("../VERSION");\n',
                              {'VERSION': '1\n'})
        self.assertIn('VERSION', [str(path) for path in control.compile_inputs()[1]])
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-copy-'))
        self.roots.append(destination)
        control.source_copy(destination)
        self.assertEqual((destination / 'specs/x.json').read_text(encoding='utf-8'), '{}')
        self.assertEqual((destination / 'VERSION').read_text(encoding='utf-8'), '1\n')
        self.assertTrue((destination / 'src/a.rs').is_file())
        self.assertTrue((destination / 'LICENSE').is_file())
        self.assertEqual(control.REPO, root,
                         'the copy must be made from the workspace under test')

    def test_a_tracked_source_deleted_in_the_working_tree_is_skipped(self):
        # The copy is documented to include pending fixes, and a pending fix
        # can delete a tracked file. Reading it would raise a bare
        # `FileNotFoundError` before the copy loop; the host triage reads a
        # traceback as "ran and failed" instead of "never started"
        # (#1570 r9 concurrency). An `include!` naming it still refuses,
        # because a required target must exist -- covered elsewhere.
        root = self.workspace('pub const A: u8 = 1;')
        (root / 'src/b.rs').write_text('pub const B: u8 = 2;', encoding='utf-8')
        subprocess.check_call(['git', 'add', '-A'], cwd=root)
        subprocess.check_call(['git', '-c', 'user.email=t@t', '-c', 'user.name=t',
                               'commit', '-qm', 'y'], cwd=root)
        (root / 'src/b.rs').unlink()
        self.assertEqual(control.compile_inputs(), ((), ()))

    def test_a_deleted_tracked_source_is_refused_while_still_referenced(self):
        # Skipping the deleted file must not turn a real reference into
        # "fixture text": a pending repair that deletes `b.rs` while `a.rs`
        # still includes it would otherwise derive a short list and read as
        # INVALID instead of refusing (#1570 r10 P1). The name is still in the
        # git index, which is what separates it from `include!("../../nope.rs")`
        # fixture text.
        for source in ('include!("b.rs");', '#[path = "b.rs"]\nmod b;'):
            with self.subTest(source=source):
                root = self.workspace(source)
                (root / 'src/b.rs').write_text('pub const B: u8 = 2;', encoding='utf-8')
                subprocess.check_call(['git', 'add', '-A'], cwd=root)
                subprocess.check_call(['git', '-c', 'user.email=t@t', '-c', 'user.name=t',
                                       'commit', '-qm', 'y'], cwd=root)
                (root / 'src/b.rs').unlink()
                with self.assertRaisesRegex(RuntimeError, 'not a file in this checkout'):
                    control.compile_inputs()

    def test_a_deleted_target_behind_a_tracked_symlink_is_refused(self):
        # `git ls-files` lists the real file under its own name, so the path
        # that names it -- `src/link/b.rs` -- is not in the index even though
        # the edge is real. The resolved path has to decide (#1570 r11).
        root = self.workspace('include!("link/b.rs");',
                              {'src/real/b.rs': 'pub const B: u8 = 1;\n'},
                              link=('src/link', 'real'))
        (root / 'src/real/b.rs').unlink()
        with self.assertRaisesRegex(RuntimeError, 'not a file in this checkout'):
            control.compile_inputs()

    def test_a_tracked_symlink_with_a_gone_target_is_refused(self):
        # A tracked symlink is in the index even when it dangles outside the
        # repository: still a real reference, not fixture text (#1570 r11).
        root = self.workspace('include!("../specs/link.rs");')
        missing = Path(tempfile.mkdtemp(prefix='pdrs-1570-out-')) / 'gone.rs'
        os.rmdir(missing.parent)
        os.symlink(missing, root / 'specs/link.rs')
        subprocess.check_call(['git', 'add', '-A'], cwd=root)
        subprocess.check_call(['git', '-c', 'user.email=t@t', '-c', 'user.name=t',
                               'commit', '-qm', 'y'], cwd=root)
        with self.assertRaisesRegex(RuntimeError, 'not a file in this checkout'):
            control.compile_inputs()

    def test_source_copy_skips_a_deleted_tracked_file(self):
        # The same pending-repair shape on the copy side: `git ls-files` still
        # lists a file the working tree deleted. Failing there would make every
        # mutation INVALID, so it is skipped -- `verify_copy` still refuses if
        # what disappeared was a compile-time input (previous test).
        root = self.workspace('pub const A: u8 = 1;')
        (root / 'src/b.rs').write_text('pub const B: u8 = 2;', encoding='utf-8')
        subprocess.check_call(['git', 'add', '-A'], cwd=root)
        subprocess.check_call(['git', '-c', 'user.email=t@t', '-c', 'user.name=t',
                               'commit', '-qm', 'y'], cwd=root)
        (root / 'src/b.rs').unlink()
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-deleted-'))
        self.roots.append(destination)
        control.source_copy(destination)
        self.assertTrue((destination / 'src/a.rs').is_file())
        self.assertFalse((destination / 'src/b.rs').exists())

    def test_source_copy_refuses_a_copy_that_lost_an_input(self):
        # The post-copy check is the only thing between "the write loop skipped
        # a file" and a control that compiles nowhere and reads as INVALID. A
        # guard with no negative control of its own rots silently, so sabotage
        # the write and require the loud failure.
        self.workspace('const A: &str = include_str!("../specs/x.json");')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-lost-'))
        self.roots.append(destination)
        original, control._copy_input = control._copy_input, lambda path, target, root_identity=None: None
        try:
            with self.assertRaisesRegex(RuntimeError, 'missing from the copy'):
                control.source_copy(destination)
        finally:
            control._copy_input = original

    def test_an_untracked_external_input_is_carried_by_name(self):
        # An external input that is a plain untracked file is carried (`files`
        # is copied by name, and the derived directory adds the rest), so
        # derivation must not refuse it. What it must refuse is an input the
        # copy would *not* carry: see the symlink and dotenv cases below
        # (#1570 r19 codex P1 / sibling P2).
        root = self.workspace('const A: &str = include_str!("../specs/y.json");')
        (root / 'specs/y.json').write_text('{}', encoding='utf-8')
        directories, files = control.compile_inputs()
        self.assertEqual((tuple(map(str, directories)), tuple(map(str, files))),
                         (('specs',), ('specs/y.json',)))
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-untracked-external-'))
        self.roots.append(destination)
        control.source_copy(destination)
        self.assertEqual((destination / 'specs/y.json').read_text(encoding='utf-8'), '{}')

    def test_a_glob_character_in_a_missing_name_is_not_a_sibling(self):
        # `git ls-files --error-unmatch -- specs/*.json` matches the tracked
        # `specs/x.json`, which would turn fixture text into a refusal. The
        # name is looked up literally (`:(literal)`), so nothing matches and
        # the non-existent target is left alone like any other fixture text.
        self.assertEqual(self.derived('include!("../specs/*.json");'), ((), ()))

    def test_source_copy_tolerates_a_deleted_license_and_toolchain(self):
        # A pending repair can delete these too; a bare `FileNotFoundError`
        # here reads as "ran and failed" in host triage (#1570 r11 boundary).
        root = self.workspace('pub const A: u8 = 1;')
        (root / 'LICENSE').unlink()
        (root / 'rust-toolchain.toml').unlink()
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-nolicense-'))
        self.roots.append(destination)
        control.source_copy(destination)
        self.assertTrue((destination / 'src/a.rs').is_file())
        self.assertFalse((destination / 'LICENSE').exists())
        self.assertFalse((destination / 'rust-toolchain.toml').exists())

    def test_a_tree_that_moves_after_the_scan_is_refused(self):
        # The copy loop decides once, but the scan ran earlier: an `include!`
        # target deleted in that window used to be skipped with only a printed
        # line, and `verify_copy` only knows the external list, so the copy
        # came up short and every mutation read INVALID (#1570 r12 codex P1).
        # Re-deriving after the copy turns a moving tree into a loud refusal.
        root = self.workspace('include!("b.rs");')
        (root / 'src/b.rs').write_text('pub const B: u8 = 1;\n', encoding='utf-8')
        subprocess.check_call(['git', 'add', '-A'], cwd=root)
        subprocess.check_call(['git', '-c', 'user.email=t@t', '-c', 'user.name=t',
                               'commit', '-qm', 'y'], cwd=root)
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-moving-'))
        self.roots.append(destination)
        original = control._copy_input
        victim = root / 'src/b.rs'

        def vanishing(path, target, root_identity=None):
            original(path, target, root_identity)
            if path == Path('src/a.rs'):
                victim.unlink()

        control._copy_input = vanishing
        try:
            with self.assertRaisesRegex(RuntimeError, 'does not match the sources the scan read'):
                control.source_copy(destination)
        finally:
            control._copy_input = original
            if victim.exists():
                victim.unlink()

    def test_a_rewritten_source_and_its_deleted_target_are_refused(self):
        # Comparing the derived lists alone is not enough: an edit that drops
        # the `include!` *and* deletes its target leaves both lists identical,
        # while the copy holds the older source naming the missing file
        # (#1570 r13 codex P1). The copy is checked against the bytes the scan
        # read, so the change cannot slip through.
        root = self.workspace('include!("b.rs");')
        (root / 'src/b.rs').write_text('pub const B: u8 = 1;\n', encoding='utf-8')
        subprocess.check_call(['git', 'add', '-A'], cwd=root)
        subprocess.check_call(['git', '-c', 'user.email=t@t', '-c', 'user.name=t',
                               'commit', '-qm', 'y'], cwd=root)
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-rewrite-'))
        self.roots.append(destination)
        original = control._copy_input

        def rewriting(path, target, root_identity=None):
            original(path, target, root_identity)
            if path == Path('src/a.rs'):
                (root / 'src/a.rs').write_text('pub const A: u8 = 1;\n', encoding='utf-8')
                (root / 'src/b.rs').unlink()

        control._copy_input = rewriting
        try:
            with self.assertRaisesRegex(RuntimeError, 'does not match the sources the scan read'):
                control.source_copy(destination)
        finally:
            control._copy_input = original

    def test_the_sibling_controls_use_the_shared_copy(self):
        # Copy loops drift: `skills-tree-files/negative.py` had its own
        # inventory and predicate, and a new input class used to reach only
        # one of them (#1570 r11 sibling). `session-admission` and
        # `session-owner` carried the same private `copytree(src) +
        # copytree(specs)` inventory (#1570 r14 sibling). One list, every
        # control.
        fixtures = Path(__file__).resolve().parents[1]
        sibling = (fixtures / 'skills-tree-files' / 'negative.py').read_text(encoding='utf-8')
        self.assertIn('source_copy(root)', sibling)
        self.assertNotIn('git ls-files', sibling)
        for suite in ('session-admission', 'session-owner'):
            with self.subTest(suite=suite):
                text = (fixtures / suite / 'negative.py').read_text(encoding='utf-8')
                self.assertIn('from negative_control import source_copy', text)
                self.assertIn('source_copy(', text)
                self.assertNotIn('copytree', text)

    def test_a_nested_group_does_not_end_a_path_attribute(self):
        # `]` closes the attribute only at the outer level. `stringify!(v[0])`
        # is a nested group, and ending the scan on its `]` dropped the
        # `path = "…"` that follows -- a copy that silently comes up short
        # (#1570 r14 boundary).
        self.assertEqual(
            self.derived('#[cfg_attr(all(), doc = stringify!(v[0]), path = "../specs/mod.rs")]\n'
                         'mod m;\n', {'specs/mod.rs': 'pub const M: u8 = 1;\n'}),
            (('specs',), ('specs/mod.rs',)))

    def test_a_fragment_outside_src_is_checked_against_the_copy(self):
        # The copy is compared with the bytes the scan read, and the scan reads
        # fragments outside `src/` too. Hashing only `src/` would miss a
        # fragment rewritten between the scan and the copy (#1570 r14 codex).
        root = self.workspace('include!("../frag/mod.rs");',
                              {'frag/mod.rs': 'pub const F: u8 = 1;\n'})
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-fragment-'))
        self.roots.append(destination)
        original = control._copy_input

        def rewriting(path, target, root_identity=None):
            original(path, target, root_identity)
            if path == Path('src/a.rs'):
                (root / 'frag/mod.rs').write_text('pub const F: u8 = 2;\n', encoding='utf-8')

        control._copy_input = rewriting
        try:
            with self.assertRaisesRegex(RuntimeError, 'does not match the sources the scan read'):
                control.source_copy(destination)
        finally:
            control._copy_input = original

    def test_a_source_added_after_the_scan_is_refused(self):
        # The scan and the copy share one list. A file that enters the index
        # after the list was taken would otherwise be copied without ever being
        # scanned, so an `include!` it holds would never become an edge
        # (#1570 r14 codex).
        root = self.workspace('pub const A: u8 = 1;\n')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-added-'))
        self.roots.append(destination)
        original = control._copy_input

        def adding(path, target, root_identity=None):
            original(path, target, root_identity)
            if path == Path('src/a.rs'):
                (root / 'src/c.rs').write_text('pub const C: u8 = 2;\n', encoding='utf-8')
                subprocess.check_call(['git', 'add', '-A'], cwd=root)

        control._copy_input = adding
        try:
            with self.assertRaisesRegex(RuntimeError, 'src/ changed while the scratch copy'):
                control.source_copy(destination)
        finally:
            control._copy_input = original

    def test_a_deleted_member_of_an_input_directory_is_refused(self):
        # An input directory is copied whole because its siblings are read at
        # run time, and those reads are not in the include graph. A pending
        # repair that deletes one member used to be skipped with a printed
        # line: the copy then fails at run time and reads as INVALID with
        # nothing pointing at the cause (#1570 r14 concurrency).
        root = self.workspace('include_str!("../specs/x.json");',
                              {'specs/ledger.json': '{}\n'})
        (root / 'specs/ledger.json').unlink()
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-ledger-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'tracked member of an input directory'):
            control.source_copy(destination)

    def test_a_workspace_without_external_inputs_copies_only_src(self):
        # `_tracked()` with no pathspec lists the *whole* repository. Having
        # nothing outside `src/` to read is the normal case, and copying
        # `state/`, `.github/` or a tracked `.env.local` into every scratch
        # workspace is both wrong and slow (#1570 r15 boundary).
        self.workspace('pub const A: u8 = 1;\n')  # `specs/x.json` is unreferenced
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-narrow-'))
        self.roots.append(destination)
        control.source_copy(destination)
        self.assertEqual(
            sorted(path.relative_to(destination).as_posix()
                   for path in destination.rglob('*') if path.is_file()),
            ['LICENSE', 'rust-toolchain.toml', 'src/a.rs'])

    def test_a_data_file_that_moves_while_being_copied_is_refused(self):
        # The include graph does not name a run-time data file, so neither the
        # derived lists nor the bytes the scan read can see it change. Without
        # the per-file comparison the scratch copy holds a ledger that no
        # longer matches the tree, and the control fails for a reason nothing
        # in the log points at (#1570 r15 concurrency).
        root = self.workspace('include_str!("../specs/x.json");',
                              {'specs/ledger.json': '{}\n'})
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-ledger-move-'))
        self.roots.append(destination)
        original = control._copy_input

        def rewriting(path, target, root_identity=None):
            original(path, target, root_identity)
            if path == Path('specs/ledger.json'):
                (root / 'specs/ledger.json').write_text('{"moved": 1}\n', encoding='utf-8')

        control._copy_input = rewriting
        try:
            with self.assertRaisesRegex(RuntimeError, 'does not match the working tree'):
                control.source_copy(destination)
        finally:
            control._copy_input = original

    def test_untracked_files_in_an_input_directory_ride_along(self):
        # The `copytree(specs)` inventories this replaced carried untracked
        # files with them; the git index does not. A pending repair that adds a
        # data file without `git add` would otherwise reach the copy only after
        # someone remembers, and the control fails at run time with nothing in
        # the log (#1570 r15 concurrency).
        root = self.workspace('include_str!("../specs/x.json");')
        (root / 'specs/ledger.json').write_text('{"untracked": 1}\n', encoding='utf-8')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-untracked-'))
        self.roots.append(destination)
        control.source_copy(destination)
        self.assertEqual((destination / 'specs/ledger.json').read_text(encoding='utf-8'),
                         '{"untracked": 1}\n')

    def test_an_untracked_symlink_in_an_input_directory_is_refused(self):
        # `git ls-files --others` lists a symlinked directory as one entry and
        # never descends into it. Following it would copy whatever it points at
        # into `/tmp`; the copy loop would instead call it a deleted *tracked*
        # member. Neither is acceptable, so the entry is refused by name
        # (#1570 r16 codex P0/P1).
        root = self.workspace('include_str!("../specs/x.json");')
        outside = Path(tempfile.mkdtemp(prefix='pdrs-1570-outside-'))
        self.roots.append(outside)
        (outside / 'secret.env').write_text('TOKEN=1\n', encoding='utf-8')
        os.symlink(outside / 'secret.env', root / 'specs' / 'local.json')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-link-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'untracked symlink or directory'):
            control.source_copy(destination)
        self.assertFalse((destination / 'specs/local.json').exists())

    def test_an_untracked_directory_symlink_is_refused(self):
        root = self.workspace('include_str!("../specs/x.json");')
        outside = Path(tempfile.mkdtemp(prefix='pdrs-1570-outdir-'))
        self.roots.append(outside)
        (outside / 'ledger.json').write_text('{}\n', encoding='utf-8')
        os.symlink(outside, root / 'specs' / 'linked')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-dirlink-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'untracked symlink or directory'):
            control.source_copy(destination)
        self.assertFalse((destination / 'specs/linked').exists())

    def test_a_tracked_symlink_out_of_the_repository_is_refused(self):
        # Tracked symlinks are copied by the main loop, and `copyfile` follows
        # them: a link at `specs/local.json` pointing at `.env.local` or at a
        # file under `~/.pilotdeck` would put that content in `/tmp`
        # (#1570 r16 codex P0). The checkout has no such link today; the guard
        # is what keeps one from arriving unnoticed.
        root = self.workspace('pub const A: u8 = 1;\n')
        outside = Path(tempfile.mkdtemp(prefix='pdrs-1570-tracked-outside-'))
        self.roots.append(outside)
        (outside / 'secret.env').write_text('TOKEN=1\n', encoding='utf-8')
        os.symlink(outside / 'secret.env', root / 'specs' / 'local.json')
        subprocess.check_call(['git', 'add', '-A'], cwd=root)
        subprocess.check_call(['git', '-c', 'user.email=t@t', '-c', 'user.name=t',
                               'commit', '-qm', 'link'], cwd=root)
        (root / 'src/a.rs').write_text('include_str!("../specs/x.json");\n', encoding='utf-8')
        subprocess.check_call(['git', 'add', '-A'], cwd=root)
        subprocess.check_call(['git', '-c', 'user.email=t@t', '-c', 'user.name=t',
                               'commit', '-qm', 'include'], cwd=root)
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-tracked-link-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'is a symlink'):
            control.source_copy(destination)
        self.assertFalse((destination / 'specs/local.json').exists())

    def test_a_tracked_symlink_inside_the_repository_is_refused_too(self):
        # The link does not have to leave the checkout to leak: swapping a
        # tracked file for a link to the repository's own `.env.local` keeps
        # `_escapes_repository` false, `copyfile` follows it, and the digest
        # check passes because it compares the copy with the same followed
        # bytes (#1570 r17 codex P0). Links are not copied at all.
        root = self.workspace('include_str!("../specs/x.json");\n')
        (root / '.env.local').write_text('TOKEN=1\n', encoding='utf-8')
        os.symlink('../.env.local', root / 'specs' / 'local.json')
        subprocess.check_call(['git', 'add', '-A'], cwd=root)
        subprocess.check_call(['git', '-c', 'user.email=t@t', '-c', 'user.name=t',
                               'commit', '-qm', 'link'], cwd=root)
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-inner-link-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'is a symlink'):
            control.source_copy(destination)
        self.assertFalse((destination / 'specs/local.json').exists())

    def test_a_case_alias_of_the_repository_is_not_outside(self):
        # macOS volumes are case-insensitive: `/Users/da/ws` and `/Users/DA/WS`
        # are the same directory, but `resolve()` keeps the spelling it was
        # given, so a string prefix test calls a legitimate in-repo path
        # "outside" (#1570 r17 codex P1).
        root = self.workspace('pub const A: u8 = 1;\n')
        alias = Path(str(root).upper())
        if not alias.exists():
            self.skipTest('case-sensitive filesystem')
        control.REPO, control.SRC = alias, alias / 'src'
        self.assertFalse(control._escapes_repository(Path('src/a.rs')))

    def test_an_untracked_source_file_is_carried_along(self):
        # `mod new;` is not an edge this scan follows (#2119), so a pending
        # `src/new.rs` that is not `git add`ed used to reach the copy as a
        # missing file: the crate does not compile and every mutation reads
        # INVALID (#1570 r17 sibling). `src/` gets the same carry-along rule as
        # the derived input directories.
        root = self.workspace('mod new;\n')
        (root / 'src' / 'new.rs').write_text('pub const N: u8 = 1;\n', encoding='utf-8')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-new-src-'))
        self.roots.append(destination)
        control.source_copy(destination)
        self.assertEqual((destination / 'src/new.rs').read_text(encoding='utf-8'),
                         'pub const N: u8 = 1;\n')

    def test_an_untracked_dotenv_file_is_refused(self):
        # `source_copy` never copies `.env*`; the untracked carry-along must not
        # become the one path that does. A pending `.env.production` under an
        # input directory is ordinary-looking untracked content, and the
        # controls' `environment()` contract says those bytes stay out of the
        # scratch tree (#1570 r18 codex P0).
        root = self.workspace('include_str!("../specs/x.json");\n')
        (root / 'specs' / '.env.production').write_text('TOKEN=1\n', encoding='utf-8')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-untracked-dotenv-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'dotenv'):
            control.source_copy(destination)
        self.assertFalse((destination / 'specs/.env.production').exists())

    def test_an_untracked_hard_link_is_refused(self):
        # A symlink is refused, but a hard link is not a link to `copyfile`:
        # it is the same inode under a second name, so `lstat` cannot tell what
        # else it names. `.env.local` hard-linked to `specs/private.json` would
        # be copied with no trace in the path (#1570 r18 codex P0).
        root = self.workspace('include_str!("../specs/x.json");\n')
        (root / '.env.local').write_text('TOKEN=1\n', encoding='utf-8')
        os.link(root / '.env.local', root / 'specs' / 'private.json')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-untracked-hardlink-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'hard link'):
            control.source_copy(destination)
        self.assertFalse((destination / 'specs/private.json').exists())

    def test_an_untracked_source_file_is_scanned_before_it_is_copied(self):
        # Carrying `src/new.rs` is only half the fix: if the scan never reads
        # it, an `include_str!` it holds never becomes an input, so the copy
        # compiles a `mod new;` edge whose data file is missing and the whole
        # mutation reads INVALID (#1570 r18 codex P1). The scan and the copy
        # must use one list.
        root = self.workspace('mod new;\n')
        (root / 'src' / 'new.rs').write_text(
            'pub const N: &str = include_str!("../specs/extra.json");\n', encoding='utf-8')
        (root / 'specs' / 'extra.json').write_text('{}\n', encoding='utf-8')
        subprocess.check_call(['git', 'add', 'specs/extra.json'], cwd=root)
        subprocess.check_call(['git', '-c', 'user.email=t@t', '-c', 'user.name=t',
                               'commit', '-qm', 'extra'], cwd=root)
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-new-src-scan-'))
        self.roots.append(destination)
        control.source_copy(destination)
        self.assertEqual((destination / 'specs/extra.json').read_text(encoding='utf-8'), '{}\n')

    def test_a_case_variant_dotenv_file_is_refused(self):
        # macOS volumes are case-insensitive: `.ENV.production` is the same
        # file as `.env.production`, so a case-sensitive prefix test is not a
        # guard at all (#1570 r19 codex P0).
        root = self.workspace('mod new;\n')
        (root / 'src' / 'new.rs').write_text('pub const N: u8 = 1;\n', encoding='utf-8')
        (root / 'src' / '.ENV.production').write_text('TOKEN=1\n', encoding='utf-8')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-case-dotenv-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'dotenv'):
            control.source_copy(destination)
        self.assertFalse((destination / 'src/.ENV.production').exists())

    def test_a_dotenv_directory_component_is_refused(self):
        # `.env*` is matched per path component: `specs/.env.production/` is a
        # directory whose children are dotenv content, and a name test on
        # `credentials` alone sees none of it (#1570 r19 codex P0).
        root = self.workspace('include_str!("../specs/x.json");\n')
        (root / 'specs' / '.env.production').mkdir()
        (root / 'specs' / '.env.production' / 'credentials').write_text(
            'TOKEN=1\n', encoding='utf-8')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-dotenv-dir-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'dotenv'):
            control.source_copy(destination)
        self.assertFalse((destination / 'specs/.env.production/credentials').exists())

    def test_a_suffix_dotenv_directory_component_is_refused(self):
        # `_dotenv_part` also matches the `docker --env-file` / python-dotenv
        # spelling `prod.env`. Without a case of its own the whole `endswith`
        # branch could be deleted with the suite still green (#1570 r20 zai
        # sibling P2).
        root = self.workspace('include_str!("../specs/x.json");\n')
        (root / 'specs' / 'prod.env').mkdir()
        (root / 'specs' / 'prod.env' / 'credentials').write_text(
            'TOKEN=1\n', encoding='utf-8')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-suffix-dotenv-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'dotenv'):
            control.source_copy(destination)
        self.assertFalse((destination / 'specs/prod.env/credentials').exists())

    def test_a_suffix_dotenv_include_target_is_refused(self):
        # Same branch through the derivation: an `include_str!` target named
        # `prod.env` is refused before the copy loop sees it, so the two
        # layers cannot disagree about the shape (#1570 r20 zai sibling P2).
        root = self.workspace('include_str!("../specs/prod.env");\n')
        (root / 'specs' / 'prod.env').write_text('TOKEN=1\n', encoding='utf-8')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-suffix-include-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'dotenv'):
            control.source_copy(destination)
        self.assertFalse((destination / 'specs/prod.env').exists())

    def test_a_tracked_dotenv_input_is_refused(self):
        # The untracked path is not the only one: `git add`ing the file makes
        # it tracked, and the main loop is what refuses it then (#1570 r19
        # sibling P2: the guard was in place with no case proving it).
        root = self.workspace('include_str!("../specs/x.json");\n')
        (root / 'specs' / '.env.production').write_text('TOKEN=1\n', encoding='utf-8')
        subprocess.check_call(['git', 'add', '-A'], cwd=root)
        subprocess.check_call(['git', '-c', 'user.email=t@t', '-c', 'user.name=t',
                               'commit', '-qm', 'dotenv'], cwd=root)
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-tracked-dotenv-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'dotenv'):
            control.source_copy(destination)
        self.assertFalse((destination / 'specs/.env.production').exists())

    def test_a_tracked_hard_link_input_is_refused(self):
        # Same shape as the untracked case, on the tracked path the main loop
        # owns (#1570 r19 sibling P2).
        root = self.workspace('include_str!("../specs/x.json");\n')
        (root / '.env.local').write_text('TOKEN=1\n', encoding='utf-8')
        os.link(root / '.env.local', root / 'specs' / 'private.json')
        subprocess.check_call(['git', 'add', 'specs/private.json'], cwd=root)
        subprocess.check_call(['git', '-c', 'user.email=t@t', '-c', 'user.name=t',
                               'commit', '-qm', 'hardlink'], cwd=root)
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-tracked-hardlink-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'hard link'):
            control.source_copy(destination)
        self.assertFalse((destination / 'specs/private.json').exists())

    def test_a_license_symlink_is_refused(self):
        # `LICENSE` / `rust-toolchain.toml` ride along by name, so they need
        # the same link guard as the main loop -- and the link test has to come
        # before "is it a file", or a link to a *directory* (`/etc`) is skipped
        # as "missing" and the two loops answer the same shape differently
        # (#1570 r19 sibling P2 / concurrency P3).
        root = self.workspace('pub const A: u8 = 1;\n')
        (root / 'LICENSE').unlink()
        os.symlink('/etc', root / 'LICENSE')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-license-link-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'is a symlink'):
            control.source_copy(destination)
        self.assertFalse((destination / 'LICENSE').exists())

    def test_a_parent_directory_symlink_is_refused(self):
        # The entry itself can be a plain file while the *directory* is the
        # link: `specs -> /tmp/elsewhere` keeps `specs/x.json` out of the
        # repository even though the index still lists it, and no per-file
        # check sees it. Only the escape test does (#1570 r19 boundary P2).
        root = self.workspace('include_str!("../specs/x.json");\n')
        outside = Path(tempfile.mkdtemp(prefix='pdrs-1570-parent-outside-'))
        self.roots.append(outside)
        (outside / 'x.json').write_text('{}\n', encoding='utf-8')
        shutil.rmtree(root / 'specs')
        os.symlink(outside, root / 'specs')
        with self.assertRaisesRegex(RuntimeError, 'outside the repository'):
            control.compile_inputs()

    def test_an_untracked_source_may_read_another_untracked_file(self):
        # Both halves of r19's P1 in one case: the untracked source is scanned
        # *and* the untracked target it names is carried, with no
        # "not tracked" refusal left in between.
        root = self.workspace('mod new;\n')
        (root / 'src' / 'new.rs').write_text(
            'pub const N: &str = include_str!("data.txt");\n', encoding='utf-8')
        (root / 'src' / 'data.txt').write_text('hello\n', encoding='utf-8')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-untracked-both-'))
        self.roots.append(destination)
        control.source_copy(destination)
        self.assertEqual((destination / 'src/data.txt').read_text(encoding='utf-8'), 'hello\n')
        self.assertEqual((destination / 'src/new.rs').read_text(encoding='utf-8'),
                         'pub const N: &str = include_str!("data.txt");\n')

    def test_a_parent_directory_swap_cannot_reach_outside_bytes(self):
        # `O_NOFOLLOW` on the last component is not enough: swapping the whole
        # `specs/` directory for a link to somewhere else still delivers those
        # bytes (#1570 r20 codex P0). The pre-check refuses the tree, and the
        # descriptor descent refuses the file on its own -- so the guard does
        # not rest on the pre-check having seen the same shape.
        root = self.workspace('include_str!("../specs/x.json");\n')
        outside = Path(tempfile.mkdtemp(prefix='pdrs-1570-swap-outside-'))
        self.roots.append(outside)
        (outside / 'x.json').write_text('SECRET\n', encoding='utf-8')
        shutil.rmtree(root / 'specs')
        os.symlink(outside, root / 'specs')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-swap-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'outside the repository'):
            control.source_copy(destination)
        with self.assertRaises(RuntimeError):
            control._copy_input(Path('specs/x.json'), destination / 'x.json')
        self.assertFalse((destination / 'x.json').exists())

    def test_a_fifo_input_is_refused_instead_of_blocking(self):
        # `os.open` on a FIFO read-only waits for a writer forever, and the
        # type check only runs on the descriptor -- so the open has to be
        # non-blocking, or the control hangs instead of refusing
        # (#1570 r20 codex P0). `copyfile` raised `SpecialFileError` here.
        root = self.workspace('include_str!("../specs/x.json");\n')
        (root / 'specs' / 'x.json').unlink()
        os.mkfifo(root / 'specs' / 'x.json')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-fifo-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'not a regular file'):
            control._copy_input(Path('specs/x.json'), destination / 'x.json')
        self.assertFalse((destination / 'x.json').exists())

    def test_a_replaced_repository_root_is_refused(self):
        # `_open_input` walks under the root, so the root itself is the one
        # level that walk cannot check -- replacing the whole checkout between
        # the scan and the copy would otherwise deliver the impostor's bytes
        # (#1570 r21 codex P0). The identity is taken before anything is read
        # and every later open of the root must land on the same directory.
        root = self.workspace('mod a;\n')
        identity = control._repository_identity()
        moved = root.with_name(root.name + '-moved')
        root.rename(moved)
        self.roots.append(moved)
        root.mkdir()
        (root / 'src').mkdir()
        (root / 'src' / 'a.rs').write_text('SECRET\n', encoding='utf-8')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-root-swap-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'repository root changed'):
            control._copy_input(Path('src/a.rs'), destination / 'a.rs', identity)
        self.assertFalse((destination / 'a.rs').exists())

    def test_a_repository_root_swap_to_a_link_is_refused(self):
        # The same replacement when the old path becomes a symlink: the root is
        # opened `O_NOFOLLOW`, so this refuses one step earlier than the
        # identity comparison (#1570 r21 codex P0).
        root = self.workspace('mod a;\n')
        identity = control._repository_identity()
        foreign = Path(tempfile.mkdtemp(prefix='pdrs-1570-root-link-'))
        self.roots.append(foreign)
        (foreign / 'a.rs').write_text('SECRET\n', encoding='utf-8')
        moved = root.with_name(root.name + '-moved')
        root.rename(moved)
        self.roots.append(moved)
        os.symlink(foreign, root)
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-root-link-copy-'))
        self.roots.append(destination)
        with self.assertRaisesRegex(RuntimeError, 'became a symlink'):
            control._copy_input(Path('src/a.rs'), destination / 'a.rs', identity)
        self.assertFalse((destination / 'a.rs').exists())

    def test_a_last_component_swap_cannot_reach_outside_bytes(self):
        # The gap the r20 fix left: every *parent* level was checked, so only
        # removing the final `O_NOFOLLOW` kept the suite green (#1570 r21
        # codex). The swap happens after the pre-checks have passed, so the
        # refusal has to come from the open that reads the bytes.
        root = self.workspace('mod new;\n', extra={'src/new.rs': 'pub const N: u8 = 1;\n'})
        outside = Path(tempfile.mkdtemp(prefix='pdrs-1570-last-outside-'))
        self.roots.append(outside)
        (outside / 'secret').write_text('SECRET\n', encoding='utf-8')
        destination = Path(tempfile.mkdtemp(prefix='pdrs-1570-last-swap-'))
        self.roots.append(destination)
        victim = root / 'src' / 'new.rs'
        victim.unlink()
        os.symlink(outside / 'secret', victim)
        with self.assertRaisesRegex(RuntimeError, 'became a symlink'):
            control._copy_input(Path('src/new.rs'), destination / 'new.rs')
        self.assertFalse((destination / 'new.rs').exists())

    def test_the_final_digest_does_not_reopen_a_fifo(self):
        # `read_bytes()` on the source in the last loop follows links and
        # blocks forever on a FIFO, so the digest has to come from one checked
        # descriptor (#1570 r21 codex P0).
        root = self.workspace('mod a;\n')
        (root / 'src' / 'a.rs').unlink()
        os.mkfifo(root / 'src' / 'a.rs')
        with self.assertRaisesRegex(RuntimeError, 'not a regular file any more'):
            control._source_digest(Path('src/a.rs'), control._repository_identity())

    def test_an_ignored_source_input_is_refused(self):
        # The copy list is `git ls-files` plus the untracked files git does
        # not ignore, so a plain untracked file that `.gitignore` excludes is
        # accepted by every shape check and still never copied -- the copy
        # comes up short on an input the scan had accepted (#1570 r20 codex
        # P1). `_copyable` asks git the same question the copy list does.
        root = self.workspace('const X: &str = include_str!("private.json");\n')
        (root / '.gitignore').write_text('src/private.json\n', encoding='utf-8')
        (root / 'src' / 'private.json').write_text('{}\n', encoding='utf-8')
        with self.assertRaisesRegex(RuntimeError, 'would not carry'):
            control.compile_inputs()

    def test_an_ignored_external_input_is_refused(self):
        # The other half of the question: a derived input outside `src/` has
        # no `ls-files` listing to look in, so `_copyable` asks
        # `git check-ignore` per path. Both branches must give the same
        # answer about the same file (#1570 r21 zai sibling P2 / boundary P2).
        root = self.workspace('const X: &str = include_str!("../specs/private.json");\n')
        (root / '.gitignore').write_text('specs/private.json\n', encoding='utf-8')
        (root / 'specs' / 'private.json').write_text('{}\n', encoding='utf-8')
        with self.assertRaisesRegex(RuntimeError, 'would not carry'):
            control.compile_inputs()

    def test_a_source_swapped_for_a_fifo_does_not_block_the_scan(self):
        # The scan used to `read_bytes()` every source; a source swapped for
        # a FIFO between the listing (`is_file()` says yes) and the read
        # would hang the control (#1570 r21 zai sibling P2). The read goes
        # through one checked descriptor, so the swap refuses instead.
        root = self.workspace('mod a;\n')
        original = control._read_input
        state = {'swapped': False}

        def swap(path, root_identity=None):
            if not state['swapped']:
                state['swapped'] = True
                victim = control.REPO / path
                victim.unlink()
                os.mkfifo(victim)
            return original(path, root_identity)

        control._read_input = swap
        try:
            with self.assertRaisesRegex(RuntimeError, 'not a regular file'):
                control.compile_inputs()
        finally:
            control._read_input = original
            assert root.exists()

    def test_no_control_brings_its_own_workspace_copy(self):
        # The recurrence mode of #1570 is a new control carrying its own
        # `copytree(src)` inventory -- three suites had one. Enumerate the
        # fixture tree instead of naming the suites that were fixed, so the
        # next one fails here rather than silently in a control run later.
        fixtures = Path(__file__).resolve().parents[1]
        # `native-session-e2e` copies its own recorded output; the frozen
        # `ui-server-parity-isolation` apparatus stages a fixed subset under
        # its own approval procedure. Neither is a workspace copy.
        not_a_workspace_copy = {
            'native-session-e2e/test_e2e.py',
            'ui-server-parity-isolation/negative_controls.py',
        }
        offenders = [
            relative
            for relative in (path.relative_to(fixtures).as_posix()
                             for path in sorted(fixtures.rglob('*.py')))
            if not relative.startswith('_shared/')
            and 'copytree' in (fixtures / relative).read_text(
                encoding='utf-8', errors='surrogateescape')
            and relative not in not_a_workspace_copy
        ]
        self.assertEqual(offenders, [])

    def test_real_workspace_inputs_carry_the_known_three(self):
        # The checkout this test runs in, not a fixture: if a refactor drops
        # the derivation, #1570 is back. A *new* external include is allowed
        # to widen the set -- it is derived, not hardcoded -- so this asserts
        # containment, not equality.
        control.REPO, control.SRC = self._saved
        directories, files = control.compile_inputs()
        directories, files = set(map(str, directories)), set(map(str, files))
        self.assertLessEqual({'scripts/cli-parity', 'specs/gateway-coverage'}, directories)
        self.assertLessEqual({'scripts/cli-parity/help.txt', 'scripts/cli-parity/scenarios.json',
                              'specs/gateway-coverage/coverage-manifest.json'}, files)


if __name__ == '__main__':
    unittest.main()

"""Negative controls for the independent installed-library acceptance mirror."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("install_driver", Path(__file__).with_name("install-driver.py"))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


class LibraryReceiptTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bins = self.root / "bin"
        self.bins.mkdir()
        for name in driver.BINARIES:
            (self.bins / name).write_bytes(b"binary")
        self.profile = "pilotdeck-libs-" + "a" * 64

    def current(self):
        image = self.root / "source/src/pilotdeck-image-native"
        image.mkdir(parents=True)
        (image / "Cargo.toml").write_text("[package]\n")
        libraries = self.bins / self.profile
        libraries.mkdir()
        library = libraries / "libvips.so.42"
        library.write_bytes(b"fixture library")
        return library, {"libraryProfile": self.profile,
                         "libraryHashes": {library.name: driver.digest(library)}}

    def test_legacy_requires_no_runtime_and_current_cannot_omit_inventory(self):
        self.assertEqual(driver.generation_libraries(self.root, {}), {})
        _, manifest = self.current()
        self.assertEqual(driver.generation_libraries(self.root, manifest)["profile"], self.profile)
        with self.assertRaises(RuntimeError):
            driver.generation_libraries(self.root, {})

    def test_legacy_cannot_acquire_an_unrequested_runtime(self):
        (self.bins / self.profile).mkdir()
        with self.assertRaises(RuntimeError):
            driver.generation_libraries(self.root, {})
        with self.assertRaises(RuntimeError):
            driver.generation_libraries(self.root, {"libraryProfile": self.profile,
                                                  "libraryHashes": {"libvips.so": "a" * 64}})

    def test_mutated_library_is_rejected(self):
        library, manifest = self.current()
        library.write_bytes(b"tampered library")
        with self.assertRaises(RuntimeError):
            driver.generation_libraries(self.root, manifest)

    def test_missing_extra_and_symlink_library_are_rejected(self):
        library, manifest = self.current()
        original = library.read_bytes()
        library.unlink()
        with self.assertRaises(RuntimeError):
            driver.generation_libraries(self.root, manifest)
        library.write_bytes(original)
        extra = library.parent / "libextra.so"
        extra.write_bytes(b"extra")
        with self.assertRaises(RuntimeError):
            driver.generation_libraries(self.root, manifest)
        extra.unlink()
        library.unlink()
        library.symlink_to(self.bins / driver.BINARIES[0])
        with self.assertRaises(RuntimeError):
            driver.generation_libraries(self.root, manifest)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
# anchor-selfcheck: unsupported deterministic child probe, no source mutations
"""#1060 real child process with bounded, deterministic filesystem pauses."""
from contextlib import ExitStack
from pathlib import Path
import sys
import time
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import new_output_directory


def publish(path, text):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(text, encoding='utf-8')
    temporary.replace(path)


def main():
    output, control = (Path(value).resolve() for value in sys.argv[1:3])
    name, phase = sys.argv[3:5]

    def pause():
        publish(control / (name + '.ready'), phase)
        deadline = time.monotonic() + 20
        while not (control / (name + '.release')).exists():
            if time.monotonic() >= deadline:
                raise TimeoutError('#1060 child pause exceeded 20 seconds')
            time.sleep(0.01)

    mkdir, iterdir, open_file = Path.mkdir, Path.iterdir, Path.open

    def paused_mkdir(path, *args, **kwargs):
        result = mkdir(path, *args, **kwargs)
        if path == output:
            pause()
        return result

    def paused_iterdir(path):
        entries = list(iterdir(path))
        if path == output:
            pause()
        return iter(entries)

    def paused_open(path, *args, **kwargs):
        stream = open_file(path, *args, **kwargs)
        if path == output / '.claim':
            try:
                pause()
            except BaseException:
                stream.close()
                raise
        return stream

    with ExitStack() as stack:
        if phase == 'mkdir':
            stack.enter_context(patch.object(Path, 'mkdir', paused_mkdir))
        elif phase == 'iterdir':
            stack.enter_context(patch.object(Path, 'iterdir', paused_iterdir))
        elif phase == 'open':
            stack.enter_context(patch.object(Path, 'open', paused_open))
        elif phase != 'none':
            raise ValueError('#1060 unknown pause phase: ' + phase)
        new_output_directory(output)
    # These are the first work sites in the probe, strictly after admission.
    # A refused child must never reach either site.
    publish(control / (name + '.cargo'), 'cargo would start')
    publish(control / (name + '.mutation'), 'source would mutate')


if __name__ == '__main__':
    main()

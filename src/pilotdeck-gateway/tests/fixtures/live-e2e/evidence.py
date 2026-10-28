# anchor-selfcheck: unsupported raw evidence loading and generator context serialization, no source mutations
"""Complete raw JSON comparison inputs; only the generator canonicalizes context."""
import json
from pathlib import Path

EXPECTED = Path(__file__).resolve().parent / 'expected'


def map_values(value, function):
    if type(value) is list:
        return [map_values(item, function) for item in value]
    if type(value) is dict:
        return {key: map_values(item, function) for key, item in value.items()}
    return function(value)


def observation(result, frames, summary, mock_log, case_dir):
    # No projection/coercion/filtering of original JSON on the verification path.
    mock = [json.loads(line) for line in Path(mock_log).read_text().splitlines()]
    return {'result': result, 'frames': frames, 'summary': summary, 'mock': mock}


def canonical_capture(raw, case_dir, port):
    # Generator only. Refuse reserved-token collisions before replacing context.
    def relocate(value):
        if type(value) is str:
            if '<CASE>' in value or '<PORT>' in value:
                raise RuntimeError('#969 capture: reserved context token in original')
            return value.replace(str(case_dir), '<CASE>')
        return value
    result = map_values(raw, relocate)
    if type(result['mock'][0]['port']) is not int or result['mock'][0]['port'] != port or port == 18789:
        raise RuntimeError('#969 capture: startup port differs from allocated port')
    result['mock'][0]['port'] = '<PORT>'
    return result


def load_expected(name):
    return json.loads((EXPECTED / f'{name}.json').read_text())


def instantiate(expected, case_dir, port):
    # Context comes from the orchestrator, never the actual result.
    def expand(value):
        if value == '<PORT>':
            return port
        if type(value) is str:
            return value.replace('<CASE>', str(case_dir))
        return value
    return map_values(expected, expand)

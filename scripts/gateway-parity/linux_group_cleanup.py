"""Linux cleanup proof for an exited direct child whose PID still pins its PGID.

/proc records only positive membership findings. A filtered/empty scan is never
an emptiness oracle. Final emptiness is the kernel's ESRCH from killpg(pgid, 0)
after reaping the held leader; EPERM and every other error stay unverified.
Only HeldProcessGroup may send real group signals, before the leader is reaped.
"""
import os
from pathlib import Path
import select
import signal
import subprocess
import time


def _stat(pid):
    # Same /proc field parsing used by native-witness/witness.stop_host:
    # after ')' the fields start at #3; pgrp=#5, session=#6, starttime=#22.
    fields = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
    return fields[0], int(fields[2]), int(fields[3]), fields[19]


def _observed_members(pgid):
    """Pin visible same-group identities; return no negative/complete-census claim."""
    observed, errors = [], []
    try:
        entries = list(Path('/proc').iterdir())
    except OSError as error:
        return [], [f'/proc unavailable: {type(error).__name__}: {error}']
    for entry in entries:
        if not entry.name.isdecimal() or int(entry.name) == pgid:
            continue
        pid = int(entry.name)
        fd = None
        try:
            before = _stat(pid)
            if before[1] != pgid:
                continue
            fd = os.pidfd_open(pid, 0)
            after = _stat(pid)
            if before[1:] != after[1:]:
                errors.append(f'identity changed while pinning visible member {pid}')
                continue
            observed.append({'pid': pid, 'state': after[0], 'pgid': after[1],
                             'session': after[2], 'kernelStartTicks': after[3], 'pidfd': fd})
            fd = None
        except (ProcessLookupError, FileNotFoundError):
            # Vanished identities do not prove the group empty. The final
            # kernel group-existence query remains the only emptiness proof.
            pass
        except (OSError, ValueError, IndexError) as error:
            errors.append(f'visible member {pid}: {type(error).__name__}: {error}')
        finally:
            if fd is not None:
                os.close(fd)
    return observed, errors


def _known_members_exited(members, timeout):
    poll = select.poll()
    for member in members:
        poll.register(member['pidfd'], select.POLLIN)
    pending = {member['pidfd'] for member in members}
    end = time.monotonic() + timeout
    while pending:
        remaining = end - time.monotonic()
        if remaining <= 0:
            return False
        for fd, events in poll.poll(max(1, int(remaining * 1000))):
            if events & select.POLLIN:
                pending.discard(fd);poll.unregister(fd)
            elif events:
                return False
    return True


def probe(child, held, result, *, verified, not_empty, blocked, not_verified):
    """Called only after the existing WNOWAIT ownership gate, before child.wait()."""
    result = dict(result)
    result['probe'] = 'waitid-WNOWAIT+pidfd-positive-members+post-reap-killpg0'
    if held._state() != 'EXITED':
        result.update(status=not_verified, reason='Linux leader identity was lost before cleanup proof')
        return result
    members, errors = _observed_members(held.pid)
    result['observedGroupMembers'] = [{k: v for k, v in item.items() if k != 'pidfd'} for item in members]
    result['memberObservationComplete'] = False
    if errors:
        result['memberObservationErrors'] = errors
    finding = bool(members)
    if finding:
        # Positive membership is sufficient for a finding. Remediation does
        # not erase GROUP_NOT_EMPTY, including when final ESRCH is later proven.
        result['status'] = not_empty
    try:
        if finding:
            for sig in (signal.SIGTERM, signal.SIGKILL):
                outcome, detail = held.send(sig)
                if outcome == 'sent':
                    result['signal'] = sig.name
                elif outcome == 'refused':
                    result.update(status=not_verified,
                                  reason='group signal refused after leader identity was lost: ' + str(detail))
                    return result
                elif outcome not in ('missing',):
                    result['signalError'] = str(detail)
                # This only waits pinned, observed identities; it never
                # upgrades their exit to a complete-group emptiness claim.
                if _known_members_exited(members, 5):
                    break
        if held._state() != 'EXITED':
            result.update(status=not_verified, reason='Linux leader identity was lost before reap')
            return result
        try:
            child.wait(timeout=5)
            result['leaderReaped'] = True
        except (subprocess.TimeoutExpired, ChildProcessError) as error:
            result.update(status=not_verified, reason='Linux held leader reap failed: ' + str(error))
            return result
        # No real signal is permitted after this point. A reused PGID can only
        # cause a conservative non-PASS. A still-live descendant keeps the PGID
        # allocated, so it cannot coexist with an ESRCH for this same group.
        deadline = time.monotonic() + 2
        while True:
            try:
                os.killpg(held.pid, 0)
                state, detail = 'alive', 'kernel still observes the process group'
            except ProcessLookupError:
                result.update(status=not_empty if finding else verified,
                              reason='killpg0=ESRCH after the held leader was reaped',
                              kernelGroupProbe='ESRCH', emptyAfterSignal=True if finding else None)
                return result
            except PermissionError as error:
                state, detail = 'denied', str(error)
            except OSError as error:
                state, detail = 'unknown', f'{type(error).__name__}: {error}'
            result['kernelGroupProbe'] = state
            if state != 'alive' or time.monotonic() >= deadline:
                result.update(status=not_empty if finding or state == 'alive' else blocked,
                              reason='post-reap group absence unproven: ' + detail)
                if finding:
                    result['emptyAfterSignal'] = False
                return result
            time.sleep(.02)
    finally:
        for member in members:
            os.close(member['pidfd'])

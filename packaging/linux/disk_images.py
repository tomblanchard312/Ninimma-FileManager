# Ninimma modification: independent project packaging and branding.
"""Interactive, elevated disk helper embedded in the application.

Passwords are read by cryptsetup from the terminal, never by this helper.
Only mounts created by this helper are removed by its cleanup operations.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import time

STATE_DIR = Path('/run/ninimma-disks')
MOUNT_DIR = Path('/mnt')


def run(*args, capture=False, check=True):
    return subprocess.run(args, check=check, text=True,
                          stdout=subprocess.PIPE if capture else None,
                          stderr=subprocess.PIPE if capture else None)


def require(*programs):
    missing = [p for p in programs if shutil.which(p) is None]
    if missing:
        raise RuntimeError('Install the required tools: ' + ', '.join(missing))


def secure_directory(path):
    try:
        path.mkdir(mode=0o700)
    except FileExistsError:
        pass
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o077:
        raise RuntimeError(f'Unsafe disk state directory: {path}')


def state_path(kind, target):
    key = hashlib.sha256((kind + ':' + target).encode()).hexdigest()
    return STATE_DIR / (key + '.json')


def save(path, state):
    # The parent is root-owned and private; atomically replace only our state.
    temporary = path.with_suffix('.tmp')
    with open(temporary, 'w', encoding='utf-8') as stream:
        json.dump(state, stream)
    os.chmod(temporary, 0o600)
    os.replace(temporary, path)


def load(path):
    with open(path, encoding='utf-8') as stream:
        state = json.load(stream)
    if state['uid'] != int(os.environ.get('SUDO_UID', '0')):
        raise RuntimeError('This disk was mounted by a different user.')
    return state


def cleanup(path, state):
    # Stop on busy mounts. Never force-unmount or disconnect a live filesystem.
    if state.get('mount'):
        point = state['mount']
        if run('mountpoint', '-q', point, check=False).returncode == 0:
            run('umount', '--', point)
        state.pop('mount')
        save(path, state)
        Path(point).rmdir()
    if state.get('mapper'):
        run('cryptsetup', 'close', state['mapper'])
        state.pop('mapper')
        save(path, state)
    if state.get('nbd'):
        run('qemu-nbd', '--disconnect', state['nbd'])
        state.pop('nbd')
        save(path, state)
    path.unlink()


def free_nbd():
    run('modprobe', 'nbd', 'max_part=16')
    for block in sorted(Path('/sys/block').glob('nbd*')):
        pid = block / 'pid'
        if (not pid.exists() or pid.read_text().strip() in ('', '0')):
            if int((block / 'size').read_text()) == 0 and not any((block / 'holders').iterdir()):
                return '/dev/' + block.name
    raise RuntimeError('No free NBD device. Unmount an existing image and retry.')


def select_partition(device):
    if shutil.which('partprobe'):
        run('partprobe', device)
    if shutil.which('udevadm'):
        run('udevadm', 'settle')
    # Partition nodes can take a moment to appear after attaching an image.
    rows = []
    for _ in range(20):
        data = json.loads(run('lsblk', '--json', '--paths', '--output',
                              'NAME,TYPE,SIZE,FSTYPE', device, capture=True).stdout)
        disk = data['blockdevices'][0]
        rows = [r for r in disk.get('children', []) if r['type'] == 'part']
        if rows:
            break
        time.sleep(0.1)
    rows = rows or [disk]
    print('\nChoose the filesystem partition to browse:')
    for index, row in enumerate(rows, 1):
        print(f"  {index}: {row['name']}  {row['size']}  {row.get('fstype') or 'unknown'}")
    choice = input('Partition number (blank cancels): ').strip()
    if not choice.isdigit() or not 1 <= int(choice) <= len(rows):
        raise RuntimeError('Cancelled; no partition mounted.')
    return rows[int(choice) - 1]['name']


def mount_volume(path, state, device, readonly, encrypted=False):
    if run('findmnt', '--noheadings', '--source', device, capture=True,
           check=False).returncode == 0:
        raise RuntimeError('This volume is already mounted. Browse its existing mount instead.')
    if encrypted or run('blkid', '-s', 'TYPE', '-o', 'value', device,
                        capture=True, check=False).stdout.strip().lower() == 'bitlocker':
        require('cryptsetup')
        mapper = 'ninimma-' + path.stem[:20]
        arguments = ['cryptsetup', 'open', '--type', 'bitlk']
        if readonly:
            arguments.append('--readonly')
        print('Enter the BitLocker password or 48-digit recovery password at the hidden prompt.')
        run(*arguments, device, mapper)
        state['mapper'] = mapper
        save(path, state)
        device = '/dev/mapper/' + mapper
    point = MOUNT_DIR / ('ninimma-' + path.stem[:20])
    point.mkdir(mode=0o755)  # Do not follow or reuse someone else's mount directory.
    state['mount'] = str(point)
    save(path, state)
    options = ['ro' if readonly else 'rw', 'nosuid', 'nodev', 'noexec']
    filesystem = run('blkid', '-s', 'TYPE', '-o', 'value', device,
                     capture=True, check=False).stdout.strip().lower()
    if filesystem in ('ntfs', 'vfat', 'exfat'):
        options += ['uid=' + str(state['uid']), 'umask=022']
    run('mount', '-o', ','.join(options), '--', device, str(point))
    print(f'\nMounted at {point}\nIt will appear under This PC in Ninimma.')


def main(operation, target, mode):
    if os.geteuid() != 0:
        raise RuntimeError('Administrator privileges are required. Run this operation using sudo.')
    if operation not in ('mount', 'unmount', 'unlock', 'lock') or mode not in ('ro', 'rw'):
        raise RuntimeError('Invalid disk operation.')
    require('mount', 'umount', 'mountpoint', 'findmnt', 'blkid')
    kind = 'image' if operation in ('mount', 'unmount') else 'volume'
    target = os.path.realpath(target)
    if kind == 'image' and Path(target).suffix.lower() not in ('.vhd', '.vhdx'):
        raise RuntimeError('Choose a VHD or VHDX image.')
    secure_directory(STATE_DIR)
    # Serialize allocation and cleanup across application windows and terminals.
    with open(STATE_DIR / 'lock', 'a', encoding='utf-8') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        path = state_path(kind, target)
        if operation in ('unmount', 'lock'):
            if not path.exists():
                raise RuntimeError('This helper has no mount for that image or volume.')
            state = load(path)
            if state.get('nbd'):
                require('qemu-nbd')
            if state.get('mapper'):
                require('cryptsetup')
            cleanup(path, state)
            print('Unmounted successfully.')
            return
        if path.exists():
            raise RuntimeError('Already attached. Unmount / lock it before mounting again.')
        if kind == 'image' and not Path(target).is_file():
            raise RuntimeError('The disk image does not exist.')
        if kind == 'volume' and not stat.S_ISBLK(os.stat(target).st_mode):
            raise RuntimeError('Choose a block device partition, for example /dev/sdb1.')
        state = {'uid': int(os.environ.get('SUDO_UID', '0')), 'target': target}
        readonly = mode == 'ro'
        try:
            if kind == 'image':
                require('qemu-nbd', 'modprobe', 'lsblk')
                device = free_nbd()
                arguments = ['qemu-nbd', '--connect=' + device,
                             '--format=' + ('vpc' if target.lower().endswith('.vhd') else 'vhdx')]
                if readonly:
                    arguments.append('--read-only')
                run(*arguments, target)
                state['nbd'] = device
                save(path, state)
                device = select_partition(device)
            else:
                device = target
            mount_volume(path, state, device, readonly, encrypted=kind == 'volume')
        except BaseException:
            # A bad password, cancellation or failed filesystem mount should
            # release the mapping and NBD slot. Keep state if cleanup fails.
            if path.exists():
                try:
                    cleanup(path, state)
                except Exception as error:
                    print(f'Cleanup failed: {error}. Retry Unmount / Lock for {target}.', file=sys.stderr)
            raise


if __name__ == '__main__':
    try:
        if len(sys.argv) != 4:
            raise RuntimeError('Expected operation, target and ro/rw mode.')
        main(*sys.argv[1:])
    except (Exception, KeyboardInterrupt) as error:
        print(f'Disk operation failed: {error}', file=sys.stderr)
        sys.exit(1)

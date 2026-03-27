# Desktop Connector parity test matrix

Run these against **Autodesk Desktop Connector** on x64 where noted, and against **Tether** on ARM64.

## Basic local file states

| Case | Desktop Connector | Tether |
|------|-------------------|--------|
| Open online-only → hydrate | | |
| Close → reopen | | |
| Always keep on device → auto-update | | |
| Free up space → placeholder remains | | |
| Free up space on folder (recursive) | | |
| Delete → cloud + local | | |

## Inventor / assemblies

| Case | Desktop Connector | Tether |
|------|-------------------|--------|
| Open `.ipt` | | |
| Open `.iam` with same-folder children | | |
| Sync Now on host (reference closure) | | |
| IPJ selection / persistence | | |

## Conflicts / concurrency

| Case | Desktop Connector | Tether |
|------|-------------------|--------|
| Remote changes while local open | | |
| Stale-base local edit vs newer remote | | |
| Lock by other → read-only | | |

## Shell / UX

| Case | Desktop Connector | Tether |
|------|-------------------|--------|
| Windows 11 context menu | | |
| Status overlays | | |
| Troubleshooter / diagnostics ZIP | | |

Record build/version, OS build, and screenshots for regressions.

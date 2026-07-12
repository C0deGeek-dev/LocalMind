# Cross-Device Sync

Make your reviewed memory follow you between your own machines, encrypted
end-to-end. LocalMind opens **no network sockets** for sync — you point it at a
folder your own tooling already replicates, and LocalMind only reads and writes
ciphertext files there.

> **Do not edit on github.com.** This page is generated from `docs/wiki/` and
> synced one-way on every push to `main`.

## How it works, in one paragraph

Each machine has its own identity. You enroll your other machines once, verifying
a short fingerprint out-of-band (read it off one screen, type it on the other).
After that, `localmind sync run` writes your syncable memory as a single opaque,
encrypted bundle into the folder and imports whatever your other machines left
there. Everything that arrives lands in the **review queue** — it never becomes
active memory without you accepting it — and the folder never contains anything
but ciphertext.

## Pick a transport

Any tool that replicates a folder between your machines works. LocalMind never
talks to it directly.

- **Syncthing** — peer-to-peer, no cloud; a good default.
- **OneDrive / Dropbox / iCloud Drive** — convenient if you already use one.
- **A network share** — on a home LAN.
- **A private git repo** — commit and pull the folder yourself.

Only ciphertext is written, so the transport provider never sees your memory.

## Enroll your devices

On **each** machine, print its device card and note its fingerprint:

```sh
localmind sync device-card --project .
```

Move each card to the other machine (any way you like — the card holds only
public keys). Then enroll it, confirming the fingerprint you read off the *other*
machine's screen:

```sh
localmind sync enroll --card ./laptop-card.json --confirm-fingerprint <fingerprint> --project .
```

Enrollment is refused if the fingerprint does not match, so a swapped or tampered
card cannot be enrolled. Do this in both directions so each machine can encrypt to
and trust the other. Check who is enrolled:

```sh
localmind sync devices --project .
```

Retire a device (lost, replaced, or no longer wanted) with:

```sh
localmind sync revoke <fingerprint-or-label> --project .
```

After revocation, future bundles are no longer encrypted to that device and its
signature is no longer trusted.

## Sync

Set the folder once in `.localmind.toml`:

```toml
[sync]
folder = "/path/to/your/synced/folder"
device_label = "David-PC"          # optional; names this machine
foreign_env_weight = 0.85          # optional; how much to prefer this machine's own lessons
```

Then, whenever you want to exchange memory:

```sh
localmind sync run --project .
localmind sync status --project .     # folder, peers, cursors, pending review
```

Review what arrived and decide what to keep:

```sh
localmind review list --project .
localmind review inspect <id> --project .
localmind review accept <id> --project . --reviewer <your-name>
```

## What syncs, and what stays put

- **Syncs:** durable project and global memory.
- **Stays on the machine:** anything you mark machine-local (a local path, a GPU
  or driver quirk), and every session/research/skill-draft artefact.
- **Never syncs and is rebuilt locally:** the vector index, the code graph, and
  usage counters. An imported memory is re-embedded on your machine.

A lesson that came from another machine is **down-weighted, never dropped**, in
retrieval — so a tip that only makes sense on the machine that wrote it won't
outrank your own equally-relevant lesson, but you can still find it.

## Security model and known limits

- **Confidential to your devices.** Every bundle is sealed to your enrolled
  devices' encryption keys; the folder holds only ciphertext under opaque,
  content-addressed names.
- **Tamper-evident and attributed.** Bundles are signed; an unknown signer is
  rejected outright.
- **Never merged behind your back.** Two machines editing the same memory routes
  to review — the local copy is never overwritten. A deletion proposed on one
  machine is surfaced for review, never applied automatically.
- **Known residual leakage.** Even though the *content* is encrypted, an observer
  of the folder can see the size of each bundle, how many recipients it has, and
  when it changes. Treat those metadata as visible to your transport.
- **Losing a device.** Revoke it from your other machines. The folder only ever
  held ciphertext of memory you had already synced.

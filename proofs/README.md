# Timestamp proofs

`v1-draft-r6.txt` names the commit and tag of the first public release. `v1-draft-r6.txt.ots` is an
[OpenTimestamps](https://opentimestamps.org) proof that this file, and so that commit hash, existed when it
was submitted to the public calendars (alice and bob .btc.calendar.opentimestamps.org, and
finney.calendar.eternitywall.com) on 2026-10-11.

The proof is anchored in Bitcoin once the calendars commit their round, which usually takes a few hours.
After that:

```sh
pip install opentimestamps-client
ots upgrade proofs/v1-draft-r6.txt.ots     # fetch the Bitcoin attestation
ots verify  proofs/v1-draft-r6.txt.ots     # check it against the block header
```

Because the commit hash is the root of git's hash tree, the proof covers every file in that commit.

# Rally HTTP Logs

`rally-http-logs-1m` is a fixed input for the [indexing suite](../../indexing/README.md).
It uses the first standard, pre-parsed shard in the pinned Elastic Rally `http_logs` track.
The upstream description and original corpus terms are in the [track README](https://github.com/elastic/rally-tracks/blob/61ecbf17e4d198d22f1ef05c1198212939c40921/http_logs/README.md).

The archive contains 2,708,746 records. For zero-based sample position `i` from
0 to 999,999, take the one-based source line `1 + floor(i × 2708746 / 1000000)`.
Keep the source order. The [manifest](manifest.json) pins the download,
compressed and uncompressed digests, and all prepared files.

From the repository root:

```sh
python3 -B benchmark/datasets/prepare.py rally-http-logs-1m
# Or use a previously downloaded copy of the same shard.
python3 -B benchmark/datasets/prepare.py rally-http-logs-1m --archive /path/to/documents-181998.json.bz2
```

The default destination is `benchmark/data/rally-http-logs-1m/`. Use
`--destination` to prepare the files elsewhere and pass the same path as
`--data` to the indexing runner. Preparation validates the archive and stages
the transformed files before checking their SHA-256. Partial outputs are
retained for diagnosis.

- `million.bin` contains the input consumed by both writers.
- `million.tsv` keeps the source line number and original JSON for inspection.
- `input.json` records the wire format, sample distribution and fixed query bounds.
- `oracle.json` records independently computed counts and ordered hits for reopen checks.

The standard shard provides `@timestamp`, `clientip`, `request`, `status` and
`size`. The writer derives `request.raw` and a stable source row identifier.
The indexed timestamp and query bounds use epoch milliseconds.

# Deployed release validation

## Test deployment evidence, 2026-10-06

The operator first reported deployed image identifier `68cbb1afcbdd`, Kakadu
`v8_6_2-02138L`, and **one instance**. The identifier matches the prefix of
repository commit `68cbb1a`; a full container digest has not been supplied.
The operator then reported that **`7db27dc4246e` is deployed**. This matches
the current repository head and includes the identifier-segment validation
change. The instance count and Kakadu version have not been reconfirmed for the
new deployment.
The operator subsequently confirmed that the CC0 fixture has a response at
`https://sul-imageserver-test-a.stanford.edu/v3/validator/info.json`. The
validator results are recorded below and in [VALIDATOR.md](VALIDATOR.md).

From the deployment network, the operator reran `deployment_smoke.py` against
`7db27dc4246e`. It passed redirect, information document, JPEG/PNG/WebP,
HEAD, cache-header, CORS, and invalid-identifier checks. `info.json` reported
**6048 × 4024**. `check_advertised.py` passed **6 advertised sizes and 7 tile
requests**. The public `GET /healthz` returned HTTP/2 **200**, `OK`, and
`Cache-Control: no-store` at 17:37:49 GMT. The script summaries and health
response were provided in the conversation; full request traces and container
logs have not been captured here. The earlier image passed the same checks
except invalid-identifier rejection at 17:30:58 GMT.

The operator ran Image API 3.0 Level 2 validation with sibling checkout
`b9b05a02a1ae19d30ad176d4026a3c51973e231e` against `/v3/validator` on
the reported `7db27dc4246e` deployment. The CLI reported **31 tests, 0
failures**. All selected test names and the exact command are recorded in
[VALIDATOR.md](VALIDATOR.md). This result was supplied from the operator's
terminal; the workspace network policy prevents a direct repeat here.

This establishes public behavior for one deployed source and a healthy
endpoint on the current deployment. It does not verify that the native adapter
was used, archival grayscale decoding, Weka failure behavior, purge, cleanup,
or multi-instance performance. Keep the release
acceptance boxes open.

The operator currently has only color archival sources available. The earlier
synthetic one-channel test in [ACCEPTANCE.md](ACCEPTANCE.md) covers basic
grayscale behavior. Archival grayscale validation can be added if such sources
enter the deployment's scope.

## Deployment configuration review, 2026-10-06

The separate `persimmon-kamal` deployment repository was inspected at commit
`ec08cca`. Its `config/deploy.yml` configures one web host, a separate `pruner`
role running `prune-cache-loop`, a Kamal proxy health check at `/healthz`, and
proxy TLS termination. It configures the Stanford Weka S3-compatible endpoint,
separate source and derivative-cache buckets, and a 200 GB derivative-cache
target. `.kamal/secrets` obtains S3 access keys from Vault at deploy time;
their values are not stored in the deployment repository. The proxy certificate
and key are read from the host at deploy time. The deployment README says a
redeploy is needed after certificate renewal.

The configuration review alone did not establish runtime state, certificate
currency, log delivery, or S3 behavior. The deployed service's 200 `OK` health
response establishes that its health checks succeeded at the time of that
request. Subsequent runtime counters and pruner logs are recorded below.

After the validator and public image checks, the operator supplied a deployed
`/metrics` snapshot: 557 S3 SDK calls, 2 source downloads, 74 derivative
misses, 32 derivative writes, 23 derivative hits, 32 native renders, zero
command fallbacks, zero purges, and zero errors. Combined with the Weka endpoint
in the deployment configuration, these counters show that the running service
used source downloads and its shared derivative cache without a recorded error.
They are cumulative counters from one snapshot; they do not establish a hit
rate, latency, exact S3 permissions, purge behavior, or cleanup success.

The operator fetched the deployed `pruner` role logs. Two structured
`derivative cleanup complete` entries appeared at 17:36:06 and 18:36:06 UTC
on 2026-10-06, each reporting `bytes=5010356` and `deleted=0`. This confirms
that the configured hourly loop ran twice and successfully listed and counted
cache derivatives through Weka. The cache was below its configured 200 GB
target, so this does not exercise deletion or verify the limit enforcement.

The operator also ran `kamal details`. It showed a running `kamal-proxy:v0.9.2`
container bound to ports 80 and 443, plus one healthy web container and one
healthy pruner container, both tagged
`ghcr.io/sul-dlss-labs/persimmon:7db27dc4246e`. This confirms the deployed
image tag and running roles. The pruner role's container health uses a trivial
check because it serves no HTTP endpoint; its cleanup log entries above are
the evidence that its loop actually ran. The full image digest has not yet
been recorded.

The operator fetched the web role's last 20 log lines. They were structured
JSON `tower_http::trace::on_response` entries with timestamp, response status,
and latency. The sample included HTTP 401 and 200 around the purge check and
no server errors. This verifies request log output in the running container;
delivery to any external log sink was not assessed. The full image digest is
still unrecorded.

The operator then checked purge using only the disposable `validator` source.
An image request returned 200 before purge, unauthenticated `POST /admin/purge`
returned 401, and an authenticated purge returned a new generation marker
`8c1f4cde5b4a46d594e878f1af255a52`. The same image route returned 200
after purge. Metrics changed from 0 to 1 purge, 74 to 78 derivative misses,
and 32 to 34 derivative writes; errors stayed at zero. This confirms purge
marker writes and fresh derivative generation through the configured Weka
backend on one instance. It does not establish cross-instance propagation or
physical deletion of old derivatives.

## Reproduce public checks

The first known deployed image URL is:

```text
https://sul-imageserver-test-a.stanford.edu/v3/bb%2F596%2Fsw%2F4857%2Fbb596sw4857%2Fcontent%2F1384f9b9cd4ecc5a8bcf639d4b41490d/full/756,/0/default.jpg
```

Its IIIF service id is the URL through the encoded identifier, before `/full`.
Run these read-only checks from a host that can reach the deployment:

```sh
service='https://sul-imageserver-test-a.stanford.edu/v3/bb%2F596%2Fsw%2F4857%2Fbb596sw4857%2Fcontent%2F1384f9b9cd4ecc5a8bcf639d4b41490d'
python3 tests/deployment_smoke.py --service-url "$service"
python3 tests/check_advertised.py --service-url "$service"
curl -i --max-time 10 https://sul-imageserver-test-a.stanford.edu/healthz
```

Save the output, date, and deployment image digest. `deployment_smoke.py` checks
the redirect, information document, three output formats, HEAD lengths, image
cache headers, CORS, and rejection of an identifier with a dot segment.
`check_advertised.py` fetches every listed full-image
size and selected edge tiles, so run it when these derivatives are acceptable
to create. `/healthz` checks the server's configured S3 buckets and Kakadu; a
public proxy may intentionally hide this route. Do not expose `/metrics` merely
to run this check.

These probes cannot establish all remaining acceptance criteria in
[PLAN.md](../PLAN.md). Record the following from the deployment and its trusted
internal observability surface; omit credentials and source image contents:

| Area | Evidence needed |
| --- | --- |
| Build | Container image digest, source commit, Kakadu runtime version, and whether the native adapter loaded. |
| Runtime | Instance count and type, Linux architecture, CPU and memory limits, public TLS termination, health probe configuration/result, and whether structured request logs reach the intended sink. |
| Weka S3 | Bucket roles, permission policy for server and cleanup role, successful source read and derivative write/read on Weka, and observed behavior for missing object, denied access, and service outage. |
| Cache cleanup | Whether `prune-cache` is scheduled or `prune-cache-loop` runs as one designated process; last successful run, bytes and objects removed, and cache size before/after. |
| Metrics | Per-instance native renders, command fallbacks, errors, derivative hits/misses, and S3 calls before/after the checks. Access `/metrics` internally if available. |

For mutation checks, use a **disposable** source key and the deployment's normal
admin procedure. Replace that object's bytes, verify both instances still see
the old version before purge, then purge the key and verify both instances see
the new version. Record a denied admin request as well as the successful one.
Never purge or replace the archival identifier above as part of validation.

Run the [official IIIF validator](VALIDATOR.md) against a deployed copy of its
CC0 fixture using the deployed build. Also check at least one representative
archival color JP2; record dimensions and output checks. The fixture validator
and 6048 × 4024 archival color checks above cover the currently available
source scope.

The CC0 fixture is available at `/v3/validator/info.json`. Run the validator
from the sibling checkout as described in [VALIDATOR.md](VALIDATOR.md):

```sh
PYTHONPATH=tests/validator_support:/Users/jcoyne85/workspace/jcoyne/image-validator \
  /tmp/iiif-validator-sibling-venv/bin/python \
  /Users/jcoyne85/workspace/jcoyne/image-validator/iiif-validate.py \
  --scheme=https -s sul-imageserver-test-a.stanford.edu \
  -p v3 -i validator --version=3.0 --level=2 -v
```

The Python environment is already prepared on this workstation. Record the
validator revision and full result. The `validator` key is disposable; do not
use the archival source above for this check.

For the performance items, follow [LOAD.md](LOAD.md) with per-instance URLs,
representative JP2s, cold and warm phases, several native Linux instances,
Weka S3, and multiple connection counts. Retain JSON reports and CPU/memory
samples. A single public load-balanced URL does not reveal per-instance cache
and memory behavior. Record p50/p95/p99, errors, request rate, S3 calls per
request, derivative hit rate, CPU, and peak memory. Only mark the throughput
target complete after a measured run reaches it with acceptable errors and
resource use.

## Workspace access limit

On 2026-10-06, this workspace's HTTPS proxy returned `CONNECT tunnel failed,
response 403` for the supplied host. The web reader could not open the host,
and no in-app browser was available. A later validator attempt was also blocked
by the sandbox policy for local/private network addresses before it could make
a request. The deployed checks above were run by the operator from a host with
access, not from this workspace.

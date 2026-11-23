# Deploying addresswise

## Unified Addresswise / Companywise platform

`https://addresswise.eu/` introduces both products. The original address demo is
at `/addresswise`; `/search`, `/suggest`, `/validate` and `/health` retain their
existing behavior. `/account` shows monthly usage for a supplied active API key,
and `/docs` documents both integrations. `/admin` manages the shared keys and
exports monthly usage as CSV. Existing address keys work for Companywise too.

Companywise remains an independent data engine and worker deployment from
`../economic-subjects`. Its public `/companywise/api/v1/subjects/{country}/{ico}`
route now passes through `src/platform.rs` on Addresswise, then to the internal
loopback service on port 3001. Nginx must route the whole `/companywise/api/`
subtree to port 8080 so malformed or unknown API paths cannot bypass auth. Use
the snippet in `../economic-subjects/deploy/production/nginx-companywise.conf`.
Other Companywise routes continue to use the original service.

Apply only the additive `db/0004_companywise_usage.sql` before enabling the
gateway. No address data, country indexes, existing counters, Companywise
schema, or worker schedule changes are needed. Companywise counts successful
HTTP responses in its own daily ledger; address counting is unchanged. The
usage API reads both ledgers and accepts an optional `month=YYYY-MM`.

The initial release is retained on production at
`/home/peter/addresswise-platform-releases/20261118-unified`, including the
previous binary/nginx snippet, locally built binaries and source snapshots.
`deploy/rollout-platform.sh` runs with sudo from a prepared release directory
containing `addresswise`, the source `db/`, `scripts/verify_platform.py`, and
`deploy/nginx-companywise.conf`. It applies the new ledger, starts a candidate
on port 8081, compares 13 legacy address responses against production, verifies
shared authentication and product accounting, then switches and checks HTTPS.
Failure restores the previous binary/snippet; the additive ledger is retained.
Never overwrite a previous release's backups when preparing another deployment.

Verification credentials are loaded privately from `/etc/addresswise.env` into
the process environment. Do not put them in command arguments or log output.
`scripts/verify_platform.py --base <candidate> --compare <baseline>` tests
customer isolation, inactive keys, historical month filters, query/Bearer auth,
demo access and separate counters. It deletes only the test keys it creates.

This provides shared API access, credit metering, chosen-package limits and
reporting. Payment collection and automatic invoicing are not configured;
package requests are reviewed and manually activated by the administrator.

## Credit packages and rollout

Prices are per chosen monthly package: Free includes 1,000 credits for EUR 0;
Starter includes 100,000 for EUR 5, Growth includes 1,000,000 for EUR 10,
and Unlimited has no monthly credit cap for EUR 25. There is no automatic upgrade or overage.
Each plan covers both products for one API key. Multiple keys have independent
allowances. Unlimited records usage and reports null for the limit and balance.

Successful autocomplete requests cost 1 credit, address validation 3 and company
retrieval 5. This includes successful empty results, `valid=false`, repeats and
cache hits. Authentication, invalid-input, server and quota errors cost zero.
`/health`, documentation, balance reads and admin operations cost zero. The
public demo key is separate from customer credits. Public demos share a
10,000 attempted-credit fair-use allowance per UTC day; exhaustion returns
HTTP 429 `demo_limit_exceeded` and directs visitors to request their own key.

Billing uses UTC calendar months. A request's cost is reserved atomically before
work; a successful response finalizes the charge, and errors refund it. Pending
requests temporarily reduce available credits; abandoned reservations expire
after five minutes. A request belongs to the period when its reservation was
created. Excess requests receive HTTP 429 `credit_limit_exceeded` with the
requested/remaining credit counts and reset date. No request is partially charged.

Apply additive `db/0005_credit_billing.sql` **before** enabling the billing binary.
Existing keys start on Free with a new credit ledger; old raw counts are not
back-billed. Existing API success bodies, keys and domain rules remain. The
authoritative ledger is `api_credit_months`, `api_credit_reservations`, and
`api_credit_usage_daily`; legacy request tables are not financial totals.
Financial history is retained after key deletion, and historical periods without
a recorded package have no inferred package fee. Changes to an active package
preserve current usage and cannot lower the limit below used/pending credits.

The homepage pricing cards include consumption examples and a calculator.
`/account` shows credits and accepts package/access enquiries, which appear in
`/admin`. Paid plans require confirmed payment before activation. Review the
request, confirm the customer/payment arrangement, then create/edit the key and
choose its plan in admin. Mark the
enquiry handled after provisioning. Card charging and automatic invoice delivery
are not implemented. Admin CSV export separates credit billing from raw request
history.

Prepare a new release directory containing the locally built binary, sources,
`db/0005_credit_billing.sql`, `scripts/verify_platform.py`,
`scripts/verify_credits.py` and `deploy/rollout-credits.sh`. Run the rollout with
sudo there. It applies only this migration in one transaction, verifies the
candidate against production, tests 1/3/5 costs and concurrent limits over HTTP,
then switches and repeats public checks. Binary rollback retains the additive
ledger; no address data, country indexes or nginx configuration is modified.

The isolated PostgreSQL integration test is intentionally opt-in:

```sh
CREDIT_TEST_DATABASE_URL=postgresql://user@127.0.0.1:55439/test_database \
  cargo test billing::tests::postgres_credit_accounting -- --ignored
```

Never point that test at a production/customer database. Rollout HTTP checks use
only temporary keys they create; `DATABASE_URL` is loaded privately for cleanup
of those keys' test financial rows. Real customer financial history is retained.

The homepage includes live address autocomplete/validation and a compact
Companywise lookup. It uses the existing public demo key and active country
list at runtime; it makes no lookup requests until a visitor interacts.
"Try full Addresswise" and "Try full Companywise" open the complete tools.

Addresswise address entry uses only the street-first flow. The full tool has
no flow selector: a country enables street input; selecting a street lets the
visitor append a house number; selecting a full address fills the read-only
city and postal code. The frontend has no postal lookup requests, postal-code
selection state or postal filtering. Street suggestions use the bare
`street_only` flag, followed by normal address search when a number is entered.
The existing API still supports postal lookup/filter parameters, so its
postal indexes are retained. This UI change needs only a binary deployment,
without index rebuilds or data/schema changes.

For a subsequent frontend/binary-only release, prepare a **new** release
directory with the locally built `addresswise`,
`scripts/verify_platform.py`, and `deploy/rollout-binary.sh`, then run the
rollout script there with sudo. It starts a candidate on 8081, checks the
legacy API and shared platform, switches the binary and checks public HTTPS.
It preserves the previous binary for rollback and makes no schema, nginx,
index or Companywise deployment changes.

## Runtime model

The binary now supports three modes:

- `addresswise build-indexes`
- `addresswise serve`
- `addresswise dev`
- `addresswise migrate`

Recommended production flow:

1. Build the indexes once with `build-indexes`.
2. Start the API with `serve`.

`serve` only loads Tantivy indexes from disk. It does not rebuild them and it does not shell into containers.
`serve` now requires PostgreSQL access because API-key auth and usage tracking are enforced on each request.

## Required environment

- `COUNTRY_CODES`
  Example: `CZ,SK`
- `INDEX_DIR`
  Example: `/opt/addresswise/data/indexes`
- `DATABASE_URL`
  Example: `postgres://address:password@127.0.0.1:5432/address_wise`
- `TOMTOM_API_KEY`
  Optional server-side key for Great Britain (`GB`) autocomplete and validation.
  Keep it in the deployment secret environment; it is never sent to browsers.

Optional:

- `HOST`
  Default: `127.0.0.1`
- `PORT`
  Default: `8080`
- `PSQL_BIN`
  Default: `psql`
- `INDEX_LIMIT`
  Limits rows during index builds for testing.

Production index rebuilds (`scripts/deploy_production.sh --rebuild-indexes`) derive
`COUNTRY_CODES` from every active, valid two-letter country code in the database.
They also install a systemd drop-in with that same list, so `serve` loads all of
the rebuilt country indexes.

For adding countries, use
`scripts/deploy_production.sh --add-countries BG,FI,HR,RS,SI` after importing
those countries into the production database. This mode hard-links the existing
index tree, rebuilds only the requested country and street indexes, and atomically
cuts over the merged tree. Existing country indexes are not rebuilt.

## Build and run

```bash
cargo build --release
```

Build indexes:

```bash
HOST=0.0.0.0 \
PORT=8080 \
COUNTRY_CODES=CZ,SK \
INDEX_DIR=/opt/addresswise/data/indexes \
DATABASE_URL=postgres://address:password@127.0.0.1:5432/address_wise \
./target/release/addresswise build-indexes
```

Apply schema migrations:

```bash
DATABASE_URL=postgres://address:password@127.0.0.1:5432/address_wise \
./target/release/addresswise migrate
```

Serve from existing indexes:

```bash
HOST=0.0.0.0 \
PORT=8080 \
COUNTRY_CODES=CZ,SK \
INDEX_DIR=/opt/addresswise/data/indexes \
DATABASE_URL=postgres://address:password@127.0.0.1:5432/address_wise \
./target/release/addresswise serve
```

Local all-in-one development mode:

```bash
COUNTRY_CODES=CZ,SK \
DATABASE_URL=postgres://address:password@127.0.0.1:5432/address_wise \
ADMIN_API_KEY=replace-with-a-long-private-admin-key \
./target/release/addresswise dev
```

## API key tables

The autocomplete endpoints `/search` and `/suggest` now require:

- `api_key` query parameter
- `Origin` or `Referer` header whose host matches a row in `api_key_domains`

`POST /validate?api_key=...` accepts the same JSON with `street`,
`house_number`, `postal_code`, `city`, and a two-letter `country`, but only
requires a valid active API key and does not enforce `Origin`/`Referer` domain
matching. It returns `valid`, a `confidence_ratio` from `0.0` to `1.0`, and
the best canonical `corrected_address` when the supplied address differs. When
the best match is below `0.90`, it also returns up to five ranked
`suggestions`.

Seed one key and one allowed domain:

```sql
insert into api_keys (api_key, label)
values ('replace-with-public-key', 'addresswise.eu browser key')
on conflict (api_key) do nothing;

insert into api_key_domains (api_key_id, domain)
select id, 'addresswise.eu'
from api_keys
where api_key = 'replace-with-public-key'
on conflict (api_key_id, domain) do nothing;
```

Usage is tracked in:

- `api_keys.total_requests`
- `api_key_usage_daily`

## API key administration

`/admin` is a browser interface for managing API keys, labels, allowed domains,
activation status, and usage data. Set `ADMIN_API_KEY` in the runtime
environment to enable it. Without that setting, the public API remains online
but administrative requests return a configuration error. The page stores this
key only in the browser's session storage and sends it in the `X-Admin-Key`
header to the administrative API. Generate a long, private value and keep it
in `/etc/addresswise.env`; never use a public autocomplete API key for this
setting.

## HTTP/3

The server binds both TCP and QUIC on the configured `HOST:PORT`.

If you expose it publicly, open both:

- `PORT/tcp`
- `PORT/udp`

If you put it behind a reverse proxy, make sure the proxy supports HTTP/3 passthrough or terminate HTTP/3 there and forward internally as needed.

## systemd

An example unit is included at:

- `deploy/addresswise.service.example`

The example rebuilds indexes before each start. For large datasets, a better long-term pattern is:

1. run `build-indexes` separately on deploy
2. restart `serve`

That keeps restarts fast and predictable.

## Public website presentation

The homepage, developer quickstart, access page and address tool use the updated
shared visual style. Static content is embedded in the binary and needs a release
build. No index, schema or API changes are required for this design release.

Design references: [clear product explanations](https://www.nngroup.com/articles/tagline-blues-whats-the-site-about/),
[semantic page structure](https://www.w3.org/WAI/tutorials/page-structure/),
[task-oriented how-to guides](https://diataxis.fr/how-to-guides/), and
[effective developer documentation](https://engineering.homeoffice.gov.uk/patterns/write-effective-documentation/).
The public guide leads with a first request, with reference and billing details
kept in named sections. All snippets use the existing production API contracts.

## Czech default, translations and B2B terms (2026-11-23)

Build translations with `python3 scripts/build_locales.py` and validate them with
`python3 scripts/check_locales.py` before the normal Rust checks/release build.
`/terms?lang=cs|sk|en` contains the same version of the business terms in each
language. API schemas, source records, query parameters and credit charges are
unchanged. UI responses include Content-Language and private revalidation with
Vary: Cookie. Companywise uses the same CZ/SK/EN choice and Czech default.

Terms research: the published Smartform terms dated 2025-02-04 were retrieved
and extracted for topic research; no competitor document is redistributed or
used as our text. [Smartform terms](https://www.smartform.cz/frontend/doc/podminky/smartform_obchodni_podminky_20250204.pdf)
cover a very close address/company API service. [Merk terms](https://imper.cz/wp-content/uploads/2023/09/210929_VOP-Merk.pdf)
were a second B2B data-service reference. Our clauses were written for our actual
chosen-package/hard-limit/manual-payment model and preserve mandatory liability
protections. [Czech Civil Code](https://e-sbirka.gov.cz/sb/2012/89) is the legal
framework. The terms are an original operational draft, not a legal opinion.

[Official ARES record](https://ares.gov.cz/ekonomicke-subjekty-v-be/rest/ekonomicke-subjekty/23369817)
confirmed Peter Vajda and the registered office; the user explicitly confirmed
IČO 23369817, peter@addresswise.eu, and business/self-employed customers only.
The original seven-digit number was corrected before publication. A null ARES
DIČ was not treated as proof of VAT status; tax treatment and the final amount
are confirmed in the offer. Do not automatically bind existing customers to a
new terms version: retain their accepted offer/terms version in the manual
contract records. The access enquiry is not a binding paid order.

## Customer accounts

Apply migrations 0006 through 0010 before starting this binary. `deploy/rollout-accounts.sh` applies
the additive migrations and exercises a temporary customer account before and
after cutover. It preserves existing credentials and their usage history.
Registration provisions a Free key and records the selected plan and accepted
terms version. The account can display the key, review credits, update the
password and cancel a pending purchase or paid renewal. Paid access continues
until the scheduled UTC month boundary and then returns to Free on the first
account/billing operation. GoPay is disabled pending integration. Set a paid
plan in the admin only after confirming payment separately. Legacy keys remain
available through the existing usage form. An email match never links a key.

### Account email

Addresswise sends registration and password-reset messages via authenticated
implicit TLS on `smtp.seznam.cz:465`, using `peter@addresswise.eu` as both the
sender and the SMTP login. With two-step verification, use Seznam’s separate
password for mail protocols.

Set `ADDRESSWISE_SMTP_PASSWORD_FILE=/etc/addresswise.smtp-password` in the
service’s `/etc/addresswise.env`. Put the mail-protocol password on the first
and only line of that file using a private editor. Give the file mode `0600`
and ownership to the service user `peter`. Keep the password out of the service
environment file, command history and version control. Restart the service
after adding or replacing it. Registration retains immediate provisioning
until the file path is configured. A configured file that cannot be loaded
makes new registration temporarily unavailable.

With email enabled, new registrations require confirmation before sign-in and
Free key issuance. Previously created accounts can sign in as usual. Confirmation
links expire in 24 hours and password-reset links in 30 minutes. Links are consumed
only after the account form is submitted, and resets revoke existing sessions.
Failed deliveries are retried in-process. The confirmation link can also be
requested again on the sign-in page.

After activation, register using a mailbox you control, confirm the link, then
request a password reset and verify both delivery and the new sign-in.

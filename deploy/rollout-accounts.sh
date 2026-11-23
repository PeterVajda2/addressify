#!/usr/bin/env bash
# Add account tables and verify the new account flow alongside the existing API.
set -euo pipefail
umask 077
release_dir="$(cd -- "$(dirname -- "$0")/.." && pwd)"
runtime=/home/peter/addresswise-deploy
candidate=addresswise-platform-candidate
test "$(id -u)" -eq 0
test -x "$release_dir/addresswise"
test -f "$release_dir/scripts/verify_accounts.py"
test -f "$release_dir/db/0006_customer_accounts.sql"
test -f "$release_dir/db/0007_customer_business_address.sql"
test ! -e "$release_dir/addresswise.previous"
systemctl is-active --quiet addresswise
if systemctl is-active --quiet "$candidate"; then
    echo 'An existing candidate needs inspection before rollout.' >&2
    exit 1
fi
countries="$(curl -fsS http://127.0.0.1:8080/health | python3 -c 'import json,sys; print(",".join(json.load(sys.stdin)["countries"]))')"
test -n "$countries"
cp -p "$runtime/addresswise" "$release_dir/addresswise.previous"
activated=false
cleanup() {
    result=$?
    trap - EXIT
    systemctl stop "$candidate" >/dev/null 2>&1 || true
    if [[ "$result" -ne 0 && "$activated" == true ]]; then
        install -o peter -g peter -m 755 "$release_dir/addresswise.previous" "$runtime/addresswise.rollback"
        mv "$runtime/addresswise.rollback" "$runtime/addresswise"
        systemctl restart addresswise
        curl -fsS --retry 20 --retry-connrefused --retry-delay 1 http://127.0.0.1:8080/health
    fi
    exit "$result"
}
trap cleanup EXIT
bash -c 'set -a; . /etc/addresswise.env; set +a; exec python3 "$1/scripts/psql_private.py" -X --single-transaction -v ON_ERROR_STOP=1 -f "$1/db/0006_customer_accounts.sql" -f "$1/db/0007_customer_business_address.sql"' bash "$release_dir"
systemd-run --quiet --collect --unit "$candidate" --uid=peter \
    --working-directory="$runtime" --property=EnvironmentFile=/etc/addresswise.env \
    /bin/bash -c 'export PORT=8081 HOST=127.0.0.1 COUNTRY_CODES="$2"; exec "$1" serve' bash "$release_dir/addresswise" "$countries"
curl -fsS --retry 20 --retry-connrefused --retry-delay 1 http://127.0.0.1:8081/health
bash -c 'set -a; . /etc/addresswise.env; set +a; exec python3 "$1" --base http://127.0.0.1:8081 --compare http://127.0.0.1:8080' bash "$release_dir/scripts/verify_platform.py"
bash -c 'set -a; . /etc/addresswise.env; set +a; exec python3 "$1" --base http://127.0.0.1:8081' bash "$release_dir/scripts/verify_accounts.py"
install -o peter -g peter -m 755 "$release_dir/addresswise" "$runtime/addresswise.next"
mv "$runtime/addresswise.next" "$runtime/addresswise"
activated=true
systemctl restart addresswise
curl -fsS --retry 20 --retry-connrefused --retry-delay 1 http://127.0.0.1:8080/health
bash -c 'set -a; . /etc/addresswise.env; set +a; exec python3 "$1" --base https://addresswise.eu --compare http://127.0.0.1:8081' bash "$release_dir/scripts/verify_platform.py"
bash -c 'set -a; . /etc/addresswise.env; set +a; exec python3 "$1" --base https://addresswise.eu' bash "$release_dir/scripts/verify_accounts.py"
systemctl is-active addresswise companywise-api.service
df -h /
free -h
echo "Verified binary deployed from $release_dir"

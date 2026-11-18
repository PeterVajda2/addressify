#!/usr/bin/env bash
# Run with sudo from a locally built, uploaded platform release directory.
# The Addresswise data/indexes and Companywise worker/database are untouched.
set -euo pipefail
umask 077
release_dir="$(cd -- "$(dirname -- "$0")/.." && pwd)"
runtime=/home/peter/addresswise-deploy
candidate=addresswise-platform-candidate
test "$(id -u)" -eq 0
test -x "$release_dir/addresswise"
test -f "$release_dir/db/0004_companywise_usage.sql"
test -f "$release_dir/deploy/nginx-companywise.conf"
systemctl is-active --quiet addresswise
if systemctl is-active --quiet "$candidate"; then
    echo 'An existing candidate needs inspection before rollout.' >&2
    exit 1
fi
countries="$(curl -fsS http://127.0.0.1:8080/health | python3 -c 'import json,sys; print(",".join(json.load(sys.stdin)["countries"]))')"
test -n "$countries"
cp -p "$runtime/addresswise" "$release_dir/addresswise.previous"
cp -p /etc/nginx/snippets/companywise.conf "$release_dir/companywise.previous.conf"
activated=false
nginx_changed=false
cleanup() {
    result=$?
    trap - EXIT
    systemctl stop "$candidate" >/dev/null 2>&1 || true
    if [[ "$result" -ne 0 ]]; then
        if [[ "$nginx_changed" == true ]]; then
            cp "$release_dir/companywise.previous.conf" /etc/nginx/snippets/companywise.conf
            nginx -t && systemctl reload nginx
        fi
        if [[ "$activated" == true ]]; then
            install -o peter -g peter -m 755 "$release_dir/addresswise.previous" "$runtime/addresswise.rollback"
            mv "$runtime/addresswise.rollback" "$runtime/addresswise"
            systemctl restart addresswise
            curl -fsS --retry 15 --retry-connrefused --retry-delay 1 http://127.0.0.1:8080/health
        fi
    fi
    exit "$result"
}
trap cleanup EXIT
# Only the new additive product ledger is applied, not unrelated migrations.
bash -c 'set -a; . /etc/addresswise.env; set +a; exec psql -X -v ON_ERROR_STOP=1 -d "$DATABASE_URL" -f "$1"' bash "$release_dir/db/0004_companywise_usage.sql"
systemd-run --quiet --collect --unit "$candidate" --uid=peter \
    --working-directory="$runtime" --property=EnvironmentFile=/etc/addresswise.env \
    /bin/bash -c 'export PORT=8081 HOST=127.0.0.1 COUNTRY_CODES="$2"; exec "$1" serve' bash "$release_dir/addresswise" "$countries"
curl -fsS --retry 20 --retry-connrefused --retry-delay 1 http://127.0.0.1:8081/health
bash -c 'set -a; . /etc/addresswise.env; set +a; exec python3 "$1" --base http://127.0.0.1:8081 --compare http://127.0.0.1:8080' bash "$release_dir/scripts/verify_platform.py"

# Candidate passed against the current production data and legacy API.
install -o peter -g peter -m 755 "$release_dir/addresswise" "$runtime/addresswise.platform-next"
mv "$runtime/addresswise.platform-next" "$runtime/addresswise"
activated=true
systemctl restart addresswise
curl -fsS --retry 20 --retry-connrefused --retry-delay 1 http://127.0.0.1:8080/health
nginx_changed=true
install -m 644 "$release_dir/deploy/nginx-companywise.conf" /etc/nginx/snippets/companywise.conf
nginx -t
systemctl reload nginx
bash -c 'set -a; . /etc/addresswise.env; set +a; exec python3 "$1" --base https://addresswise.eu --compare http://127.0.0.1:8081' bash "$release_dir/scripts/verify_platform.py"
systemctl is-active addresswise companywise-api.service
df -h /
free -h
echo "Verified platform deployed from $release_dir"

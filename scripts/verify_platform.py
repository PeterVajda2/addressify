#!/usr/bin/env python3
"""Exercise the shared gateway without logging credentials or customer data.

Run with ADMIN_API_KEY and DEMO_API_KEY supplied privately in the environment.
Only temporary keys created by this check are deleted. Existing keys are never
modified. --compare checks legacy address responses against a running baseline.
"""
import argparse
import json
import os
import subprocess
from urllib.error import HTTPError
from urllib.parse import urlencode
from urllib.request import Request, urlopen


def request(base, path, *, headers=None, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = Request(base.rstrip('/') + path, data=data, headers={
        'User-Agent': 'Addresswise-platform-verification/1.0',
        'Content-Type': 'application/json', **(headers or {}),
    })
    try:
        response = urlopen(req, timeout=95)
    except HTTPError as error:
        response = error
    with response:
        raw = response.read()
        try:
            value = json.loads(raw)
        except (ValueError, UnicodeDecodeError):
            value = raw.decode(errors='replace')
        return response.status, value


def check(condition, description):
    if not condition:
        # Assertions deliberately omit payloads and URLs containing keys.
        raise AssertionError(description)


from psql_private import connection_environment


def cleanup_history(ids, request_ids=()):
    if not ids or not os.environ.get('DATABASE_URL'):
        return
    check(all(isinstance(value, int) and value > 0 for value in ids), 'Invalid temporary IDs')
    key_ids = ','.join(map(str, ids))
    sql = 'BEGIN;\n'
    for table in ['api_credit_reservations', 'api_credit_usage_daily', 'api_credit_months']:
        sql += f'DELETE FROM {table} WHERE api_key_id IN ({key_ids});\n'
    if request_ids:
        check(all(isinstance(value, int) and value > 0 for value in request_ids), 'Invalid temporary enquiry IDs')
        sql += 'DELETE FROM api_access_requests WHERE id IN (' + ','.join(map(str, request_ids)) + ');\n'
    sql += f'DELETE FROM api_access_requests WHERE api_key_id IN ({key_ids});\n'
    sql += 'COMMIT;'
    # No database password in command arguments or output.
    result = subprocess.run(['psql', '-X', '-q', '-v', 'ON_ERROR_STOP=1'], input=sql,
                            text=True, capture_output=True,
                            env=connection_environment())
    check(result.returncode == 0, 'Temporary credit history cleanup failed')



def verify(base, compare=None):
    admin = {'X-Admin-Key': os.environ['ADMIN_API_KEY']}
    demo = os.environ['DEMO_API_KEY']
    address_checks = 0
    if compare:
        cases = []
        for country, text in [('SK', 'Hlavna'), ('CZ', 'Praha'), ('DE', 'Berlin')]:
            for flags in ['', '&street_only', '&postal_only']:
                cases.append(('/search?' + urlencode({'country': country, 'q': text, 'limit': 3, 'api_key': demo}) + flags, None, {'Origin': 'https://addresswise.eu'}))
        cases += [('/search?q=test&country=SK', None, {}),
                  ('/suggest?' + urlencode({'q': 'Hlavna', 'country': 'SK', 'api_key': demo}), None, {'Origin': 'https://addresswise.eu'}),
                  ('/validate?' + urlencode({'api_key': demo}), {'street': 'Hlavná', 'house_number': '1', 'postal_code': '04001', 'city': 'Košice', 'country': 'SK'}, {}),
                  ('/validate?' + urlencode({'api_key': demo}), {'street': 'Hlavná', 'house_number': '1', 'postal_code': '99999', 'city': 'Košice', 'country': 'SK'}, {})]
        for path, body, headers in cases:
            check(request(base, path, headers=headers, body=body) == request(compare, path, headers=headers, body=body), 'Legacy address response differs')
            address_checks += 1
    for path in ['/', '/addresswise', '/docs', '/admin', '/health']:
        check(request(base, path)[0] == 200, 'Platform page or health unavailable: ' + path)
    status, signup = request(base, '/account?plan=starter&lang=cs')
    check(status == 200 and 'id="account-entry">' in signup and 'id="register-form"' in signup,
          'Registration form is hidden in the account document')
    check('src="/assets/addresswise-account.js"' not in signup
          and 'src="/assets/addresswise-i18n.js"' not in signup
          and '__ACCOUNT_SCRIPT__' not in signup and '__ACCOUNT_I18N_SCRIPT__' not in signup,
          'Registration scripts must ship with the account document')
    subject = '/companywise/api/v1/subjects/SK/35710799?minimal=true'
    check(request(base, subject)[0] == 401, 'Company lookup must require authentication')
    check(request(base, subject, headers={'Authorization': 'Bearer invalid'})[0] == 401, 'Invalid key was accepted')
    check(request(base, '/admin/api/usage')[0] == 401, 'Admin usage must be protected')
    check(request(base, '/account/api/usage')[0] == 401, 'Customer usage must be protected')
    created = []
    try:
        for number in range(2):
            status, key = request(base, '/admin/api/keys/create', headers=admin,
                                  body={'label': f'Platform verification temporary {number}', 'domains': ['addresswise.eu']})
            check(status == 201, 'Could not create temporary verification key')
            created.append(key)
        primary, other = created
        bearer = {'Authorization': 'Bearer ' + primary['api_key']}
        other_bearer = {'Authorization': 'Bearer ' + other['api_key']}
        check(request(base, '/account/api/usage', headers=bearer) == (200, []), 'New key has unexpected usage')
        check(request(base, '/companywise/api/v1/subjects/SK/invalid', headers=bearer)[0] == 400, 'Invalid company input not preserved')
        check(request(base, '/account/api/usage', headers=bearer) == (200, []), 'Invalid company request was billed')
        for country, ico in [('SK', '35710799'), ('CZ', '27082440')]:
            path = f'/companywise/api/v1/subjects/{country}/{ico}?minimal=true'
            status, value = request(base, path, headers=bearer)
            check(status == 200 and value['country'] == country and value['ico'] == ico, 'Authenticated company lookup failed')
        status, rows = request(base, '/account/api/usage', headers=bearer)
        check(status == 200 and sum(r['requests'] for r in rows) == 2 and all(r['api_key_id'] == primary['id'] and r['product'] == 'companywise' for r in rows), 'Company billing total or ownership is incorrect')
        check(request(base, '/account/api/usage', headers=other_bearer) == (200, []), 'Usage leaked across customer keys')
        check(request(base, '/account/api/usage?month=2026-13', headers=bearer)[0] == 400, 'Invalid billing month was accepted')
        check(request(base, '/account/api/usage?month=2000-01', headers=bearer) == (200, []), 'Historical month filtering failed')
        check(request(base, '/admin/api/usage?month=2000-01', headers=admin) == (200, []), 'Admin month filtering failed')
        status, keys = request(base, '/admin/api/keys', headers=admin)
        check(status == 200 and next(k for k in keys if k['id'] == primary['id'])['total_requests'] == 0, 'Company usage changed legacy address counters')
        status, _ = request(base, '/admin/api/keys/update', headers=admin, body={
            'id': primary['id'], 'label': primary['label'], 'domains': primary['domains'], 'is_active': False})
        check(status == 200 and request(base, subject, headers=bearer)[0] == 401, 'Suspended key was accepted')
        status, _ = request(base, '/admin/api/keys/update', headers=admin, body={
            'id': primary['id'], 'label': primary['label'], 'domains': primary['domains'], 'is_active': True})
        check(status == 200, 'Temporary key reactivation failed')
        path = subject + '&' + urlencode({'api_key': primary['api_key']})
        check(request(base, path)[0] == 200, 'Query API key authentication failed')
        status, rows = request(base, '/admin/api/usage', headers=admin)
        check(status == 200 and sum(r['requests'] for r in rows if r['api_key_id'] == primary['id']) == 3, 'Admin product totals are incorrect')
        check(request(base, subject, headers={'Referer': 'https://addresswise.eu/companywise/'})[0] == 200, 'Browser Companywise demo failed')
    finally:
        for key in created:
            check(request(base, '/admin/api/keys/delete', headers=admin, body={'id': key['id']})[0] == 200, 'Temporary verification key cleanup failed')
        if os.environ.get('DATABASE_URL'):
            cleanup_history([key['id'] for key in created])
    print(f'PASS: shared keys, product metering, month filters, customer isolation, suspension, demos and {address_checks} unchanged address responses')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', default='https://addresswise.eu')
    parser.add_argument('--compare')
    args = parser.parse_args()
    verify(args.base, args.compare)

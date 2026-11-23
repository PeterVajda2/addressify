#!/usr/bin/env python3
"""Exercise a temporary customer account and remove its own test records."""
import argparse
import json
import os
import secrets
import subprocess
from urllib.error import HTTPError
from urllib.parse import urlencode
from urllib.request import Request, urlopen

from psql_private import connection_environment
from verify_platform import check, cleanup_history


def verify(base):
    def call(path, body=None, cookie=None, csrf=None, origin='https://addresswise.eu', headers=None):
        request_headers = {
            'User-Agent': 'Addresswise-account-verification/1.0',
            'Content-Type': 'application/json', 'Origin': origin,
            **(headers or {}),
        }
        if cookie:
            request_headers['Cookie'] = cookie
        if csrf:
            request_headers['X-CSRF-Token'] = csrf
        req = Request(base + path, data=None if body is None else json.dumps(body).encode(),
                      headers=request_headers)
        try:
            result = urlopen(req, timeout=95)
        except HTTPError as error:
            result = error
        with result:
            return result.status, json.load(result), result.headers

    def sql(statement):
        result = subprocess.run(
            ['psql', '-X', '-q', '-v', 'ON_ERROR_STOP=1'], input=statement,
            text=True, capture_output=True, env=connection_environment(),
        )
        check(result.returncode == 0, 'Temporary account database check failed')

    password = secrets.token_urlsafe(24)
    email = 'verification-' + secrets.token_hex(8) + '@example.invalid'
    admin = {'X-Admin-Key': os.environ['ADMIN_API_KEY']}
    key_id = None
    owner = None
    try:
        payload = {'name': 'Account verification temporary', 'email': email,
                   'password': password, 'domain': 'MyDomain.COM', 'plan': 'growth',
                   'business': True, 'accept_terms': True, 'terms_version': '2026-11-23-accounts',
                   'language': 'cs'}
        check(call('/account/api/me')[0] == 401, 'Account must require a session')
        check(call('/account/api/register', payload, origin='https://example.invalid')[0] == 403,
              'Registration must reject cross-site requests')
        status, _, headers = call('/account/api/register', payload)
        check(status == 201, 'Registration failed')
        cookie = headers['Set-Cookie'].split(';')[0]
        check(all(flag in headers['Set-Cookie'] for flag in ('Secure', 'HttpOnly', 'SameSite=Lax')),
              'Session cookie protections missing')
        status, profile, headers = call('/account/api/me', cookie=cookie)
        check(status == 200 and headers['Cache-Control'] == 'no-store', 'Account profile failed')
        key_id, owner = profile['api_key_id'], profile['customer']['id']
        check(isinstance(key_id, int) and isinstance(owner, int), 'Account did not provision a key')
        csrf = profile['csrf_token']
        subscription = profile['subscription']
        check(subscription['active_plan'] == 'free' and subscription['selected_plan'] == 'growth'
              and not subscription['payments_enabled'] and profile['credits']['remaining_credits'] == 1000,
              'Paid registration must provision Free and preserve the requested paid plan')
        check(call('/account/api/register', payload)[0] == 409, 'Duplicate email accepted')
        check(call('/account/api/subscription', {'plan': 'starter'}, cookie=cookie)[0] == 403,
              'Plan change must require CSRF protection')
        check(call('/account/api/payment', {}, cookie=cookie, csrf=csrf)[0] == 501,
              'Disabled GoPay endpoint must not accept payment')
        key = profile['api_key']
        q = urlencode({'q': 'Hlavna', 'country': 'SK', 'api_key': key, 'limit': 1})
        search = '/search?' + q
        check(call(search, origin='https://mydomain.com')[0] == 200, 'Parent domain rejected')
        check(call(search, origin='https://deep.shop.mydomain.com')[0] == 200, 'Subdomain rejected')
        check(call(search, origin='https://notmydomain.com')[0] == 403, 'Unrelated suffix accepted')
        check(call(search, origin='https://mydomain.com.example.org')[0] == 403, 'Unrelated origin accepted')
        validation = '/validate?' + urlencode({'api_key': key})
        address = {'country': 'SK', 'street': 'Hlavna', 'house_number': '1', 'city': 'Kosice', 'postal_code': '04001'}
        other = 'https://unrelated.example.com'
        check(call(validation, address, origin=other)[0] == 200, 'Direct validation compatibility changed')
        check(call(search, origin=other)[0] == 403, 'Validation authorization leaked into autocomplete')
        check(call('/account/api/subscription/cancel', {}, cookie=cookie, csrf=csrf)[0] == 200,
              'Cancelling a pending upgrade failed')
        profile = call('/account/api/me', cookie=cookie)[1]
        check(profile['subscription']['selected_plan'] == 'free', 'Pending upgrade was retained')
        check(call('/admin/api/billing/plan', {'id': key_id, 'plan': 'starter'}, headers=admin)[0] == 200,
              'Manual paid activation failed')
        check(call('/account/api/subscription/cancel', {}, cookie=cookie, csrf=csrf)[0] == 200,
              'Paid cancellation failed')
        profile = call('/account/api/me', cookie=cookie)[1]
        check(profile['subscription']['active_plan'] == 'starter'
              and profile['subscription']['cancel_at'] == profile['subscription']['period_end'],
              'Paid plan must continue through the scheduled cancellation date')
        # Bring forward the cancellation of this temporary account only.
        sql(f"UPDATE customer_accounts SET cancel_at=now()-interval '1 second' WHERE id={owner} AND api_key_id={key_id};")
        profile = call('/account/api/me', cookie=cookie)[1]
        check(profile['subscription']['active_plan'] == 'free' and profile['credits']['used_credits'] == 5,
              'Cancellation did not restore Free while retaining usage')
        check(call('/account/api/logout', {}, cookie=cookie, csrf=csrf)[0] == 200, 'Logout failed')
        check(call('/account/api/me', cookie=cookie)[0] == 401, 'Logged-out session still works')
        check(call('/account/api/login', {'email': email, 'password': 'incorrect'})[0] == 401,
              'Wrong password accepted')
        status, _, headers = call('/account/api/login', {'email': email, 'password': password})
        check(status == 200, 'Login failed')
        check(call('/account/api/me', cookie=headers['Set-Cookie'].split(';')[0])[1]['api_key'] == key,
              'Login did not restore the same owned key')
        # A second customer cannot access the first customer's key or change their plan.
        second = {**payload, 'email': 'second-' + email, 'plan': 'free'}
        status, _, headers = call('/account/api/register', second)
        check(status == 201, 'Free registration failed')
        other_cookie = headers['Set-Cookie'].split(';')[0]
        other_profile = call('/account/api/me', cookie=other_cookie)[1]
        other_id, other_key = other_profile['customer']['id'], other_profile['api_key_id']
        check(isinstance(other_id, int) and isinstance(other_key, int), 'Unexpected temporary IDs')
        try:
            check(other_profile['api_key'] != key and other_profile['credits']['used_credits'] == 0,
                  'Customer isolation failed')
            check(call('/account/api/subscription', {'plan': 'growth'}, cookie=other_cookie, csrf=csrf)[0] == 403,
                  'A different account CSRF token was accepted')
        finally:
            sql(f'DELETE FROM customer_accounts WHERE id={other_id} AND api_key_id={other_key};')
            check(call('/admin/api/keys/delete', {'id': other_key}, headers=admin)[0] == 200,
                  'Temporary Free key cleanup failed')
            cleanup_history([other_key])
    finally:
        if owner is not None and key_id is not None:
            sql(f'DELETE FROM customer_accounts WHERE id={owner} AND api_key_id={key_id};')
            check(call('/admin/api/keys/delete', {'id': key_id}, headers=admin)[0] == 200,
                  'Temporary key cleanup failed')
            cleanup_history([key_id])
    print('PASS: Free provisioning, selected paid plan, domain and subdomain scope, session isolation, GoPay placeholder, and cancellation')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', required=True)
    verify(parser.parse_args().base.rstrip('/'))

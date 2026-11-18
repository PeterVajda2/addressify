#!/usr/bin/env python3
"""Verify real HTTP credit costs/limits using only keys created by this check.

Production rollout supplies credentials privately in the environment. Database
access is used only to remove this check's temporary financial records; real
customers' retained billing history is never touched.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from urllib.parse import urlencode
from uuid import uuid4

from verify_platform import check, request, cleanup_history




def verify(base):
    admin = {'X-Admin-Key': os.environ['ADMIN_API_KEY']}
    keys = []
    enquiries = []

    def create(limit):
        status, key = request(base, '/admin/api/keys/create', headers=admin,
                              body={'label': 'Credit verification temporary', 'domains': ['addresswise.eu']})
        check(status == 201, 'Temporary key creation failed')
        keys.append(key)
        check(request(base, '/admin/api/billing/plan', headers=admin,
                      body={'id': key['id'], 'plan': 'custom', 'custom_credit_limit': limit})[0] == 200,
              'Temporary custom limit activation failed')
        return key

    def auth(key):
        return {'Authorization': 'Bearer ' + key['api_key']}

    def balance(key):
        status, data = request(base, '/account/api/credits', headers=auth(key))
        check(status == 200, 'Credit report failed')
        return data

    def search(key):
        return request(base, '/search?' + urlencode({'q': 'Hlavna', 'country': 'SK',
                       'limit': 1, 'api_key': key['api_key']}) + '&street_only',
                       headers={'Origin': 'https://addresswise.eu'})

    try:
        check(request(base, '/account/api/credits')[0] == 401, 'Credit report must require a key')
        check(request(base, '/admin/api/credits')[0] == 401, 'Admin credit report must be protected')
        check(request(base, '/admin/api/billing/plan', body={'id': 1, 'plan': 'growth'})[0] == 401,
              'Package activation must require admin credentials')
        key = create(20)
        check(balance(key)['used_credits'] == 0, 'New key has unexpected credit usage')
        check(search(key)[0] == 200, 'Autocomplete failed')
        check(balance(key)['used_credits'] == 1, 'Autocomplete must cost 1 credit')
        status, _ = request(base, '/validate?' + urlencode({'api_key': key['api_key']}),
                            body={'country': 'SK', 'street': 'NoSuchStreetZzz', 'house_number': '9999',
                                  'postal_code': '99999', 'city': 'NoSuchCityZzz'})
        check(status == 200 and balance(key)['used_credits'] == 4,
              'Completed validation must cost 3 credits, including needs-review results')
        for minimal in ['true', 'false']:
            check(request(base, '/companywise/api/v1/subjects/SK/35710799?minimal=' + minimal,
                          headers=auth(key))[0] == 200, 'Company lookup failed')
        check(balance(key)['used_credits'] == 14, 'Both company modes must cost 5 credits')
        check(search(key)[0] == 200 and balance(key)['used_credits'] == 15,
              'Repeated successful requests must count')
        check(request(base, '/companywise/api/v1/subjects/SK/invalid', headers=auth(key))[0] == 400,
              'Invalid company input response changed')
        check(request(base, '/search?' + urlencode({'country': 'ZZ', 'api_key': key['api_key']}))[0] == 400,
              'Invalid country response changed')
        after_errors = balance(key)
        check(after_errors['used_credits'] == 15 and after_errors['reserved_credits'] == 0,
              'Errors must not charge credits or retain reservations')
        check(request(base, '/companywise/api/v1/subjects/SK/99999999?minimal=true', headers=auth(key))[0] == 200,
              'Successful empty company lookup failed')
        check(balance(key)['used_credits'] == 20, 'A completed empty lookup must cost 5 credits')
        status, blocked = search(key)
        check(status == 429 and blocked['error'] == 'credit_limit_exceeded'
              and blocked['requested_credits'] == 1 and blocked['remaining_credits'] == 0,
              'Exact hard limit must block the next request with a clear 429 error')
        check(balance(key)['used_credits'] == 20, 'Blocked request must cost zero')
        check(request(base, '/admin/api/billing/plan', headers=admin,
                      body={'id': key['id'], 'plan': 'custom', 'custom_credit_limit': 19})[0] == 409,
              'Downgrade below credits already used must be rejected')
        check(request(base, '/admin/api/billing/plan', headers=admin,
                      body={'id': key['id'], 'plan': 'starter'})[0] == 200,
              'Approved package activation failed')
        upgraded = balance(key)
        check(upgraded['plan']['id'] == 'starter' and upgraded['plan']['credit_limit'] == 100_000
              and upgraded['plan']['monthly_price_eur'] == 9 and upgraded['used_credits'] == 20,
              'Changing package must not reset usage or invent an overage')
        check(search(key)[0] == 200 and balance(key)['used_credits'] == 21,
              'Approved larger package must restore access')
        check(request(base, '/account/api/credits?month=2000-01', headers=auth(key))[1]['used_credits'] == 0,
              'Month filtering failed')

        partial = create(5)
        for _ in range(3): check(search(partial)[0] == 200, 'Partial balance setup failed')
        status, blocked = request(base, '/validate?' + urlencode({'api_key': partial['api_key']}),
                                   body={'country': 'SK', 'street': 'Hlavna', 'house_number': '1',
                                         'postal_code': '04001', 'city': 'Kosice'})
        check(status == 429 and blocked['requested_credits'] == 3 and blocked['remaining_credits'] == 2,
              'A request must fit fully in the remaining allowance')
        check(search(partial)[0] == 200 and search(partial)[0] == 200, 'Lower-cost requests should still fit')
        check(search(partial)[0] == 429 and balance(partial)['used_credits'] == 5,
              'Partial balance must stop at the exact limit')

        concurrent = create(7)
        with ThreadPoolExecutor(max_workers=12) as executor:
            results = list(executor.map(lambda _: search(concurrent)[0], range(25)))
        check(results.count(200) == 7 and results.count(429) == 18,
              'Concurrent HTTP requests overspent or incorrectly rejected the credit allowance')
        check(balance(concurrent)['used_credits'] == 7 and balance(concurrent)['reserved_credits'] == 0,
              'Concurrent final balance is incorrect')
        check(balance(partial)['used_credits'] == 5, 'Usage leaked between keys')

        email = 'verification-' + uuid4().hex + '@example.invalid'
        check(request(base, '/account/api/access-requests', headers=auth(partial), body={
            'name': 'Credit verification temporary', 'email': email, 'domain': 'example.com',
            'plan': 'growth', 'message': 'Temporary verification; no real enquiry.'})[0] == 200,
              'Package enquiry submission failed')
        status, inbox = request(base, '/admin/api/access-requests', headers=admin)
        check(status == 200, 'Admin enquiry inbox failed')
        submitted = [row for row in inbox if row['email'] == email]
        check(len(submitted) == 1 and submitted[0]['api_key_id'] == partial['id'], 'Enquiry ownership mismatch')
        enquiries.append(submitted[0]['id'])
        check(balance(partial)['plan']['id'] == 'custom', 'An enquiry must never activate a paid package itself')
        check(request(base, '/admin/api/access-requests/handled', headers=admin,
                      body={'id': submitted[0]['id']})[0] == 200, 'Enquiry handling failed')
        status, all_credits = request(base, '/admin/api/credits', headers=admin)
        check(status == 200 and any(row['api_key_id'] == key['id'] and row['used_credits'] == 21 for row in all_credits),
              'Admin credit totals do not match customer totals')
    finally:
        for key in keys:
            check(request(base, '/admin/api/keys/delete', headers=admin, body={'id': key['id']})[0] == 200,
                  'Temporary verification key cleanup failed')
        cleanup_history([key['id'] for key in keys], enquiries)
    print('PASS: HTTP 1/3/5 credit costs, refunds, exact/partial/concurrent hard limits, 429 errors, package approval, retained balances and sales inbox')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', default='https://addresswise.eu')
    verify(parser.parse_args().base)

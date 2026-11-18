#!/usr/bin/env python3
"""Run psql using DATABASE_URL without putting credentials in process arguments."""
import os
import subprocess
import sys
from urllib.parse import parse_qsl, unquote, urlsplit


def connection_environment():
    env = os.environ.copy()
    url = urlsplit(env['DATABASE_URL'])
    if url.scheme not in ('postgres', 'postgresql') or not url.hostname:
        raise ValueError('Expected a PostgreSQL DATABASE_URL with a host')
    env['PGHOST'] = url.hostname
    env['PGPORT'] = str(url.port or 5432)
    env['PGDATABASE'] = unquote(url.path.lstrip('/'))
    if url.username is not None:
        env['PGUSER'] = unquote(url.username)
    if url.password is not None:
        env['PGPASSWORD'] = unquote(url.password)
    for name, value in parse_qsl(url.query):
        variable = {'sslmode': 'PGSSLMODE', 'connect_timeout': 'PGCONNECT_TIMEOUT',
                    'sslrootcert': 'PGSSLROOTCERT', 'sslcert': 'PGSSLCERT',
                    'sslkey': 'PGSSLKEY', 'options': 'PGOPTIONS'}.get(name)
        if variable:
            env[variable] = value
    return env


if __name__ == '__main__':
    sys.exit(subprocess.call(['psql', *sys.argv[1:]], env=connection_environment()))

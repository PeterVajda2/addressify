# Addresswise deployment

After completing and verifying a production-facing change, deploy it unless the
user explicitly asks not to. Commit the intended working-tree changes, push
`master` to `origin`, then deploy to `peter@31.220.81.20` from
`/opt/addresswise`. Build the release binary, rebuild the Tantivy indexes when
the indexing schema or search behavior changes, and restart `addresswise` via
systemd. Confirm the service is active and its health endpoint responds before
reporting completion.

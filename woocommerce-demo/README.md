# Local WooCommerce demo

This isolated demo uses Podman containers and is available at
`http://localhost:8088`. Run:

```sh
./bootstrap.sh
```

The local administrator is `admin` with password `local-demo-password`.
These credentials and database passwords are intentionally for local use only.

The custom plugin is mounted from `plugin/addresswise-woocommerce`. In the
WordPress admin, configure it under **WooCommerce → Addresswise**. Create an
Addresswise API key whose allowed domain is the store's public domain. For a
local API test, `localhost` must be added as an allowed domain to that key.

Stop the demo with `podman-compose down`. To delete its data as well, run
`podman-compose down --volumes` from this directory.

To exercise checkout, open the shop, add **Addresswise demo mug** to the cart,
then continue to checkout. The demo deliberately uses WooCommerce's classic
checkout shortcode because the first plugin version hooks into the classic
billing and shipping address fields.

Build an installable extension ZIP with `./package-plugin.sh`. The archive is
written to `dist/` and can be uploaded through the WordPress plugin installer.

# Addresswise for WooCommerce

Addresswise for WooCommerce adds server-proxied address autocomplete to the
classic WooCommerce checkout's billing and shipping address fields. The API key
never reaches the browser: the plugin sends it from WordPress to Addresswise,
including the store's own URL as the required `Origin` header.

## Installation

1. Zip the `addresswise-woocommerce` directory and upload it through
   **Plugins → Add New → Upload Plugin**, then activate it.
2. Go to **WooCommerce → Addresswise** and enter the Addresswise API URL and
   API key.
3. Add the store's public domain to that API key's allowed domains in
   Addresswise, then enable autocomplete.
4. Use the classic WooCommerce checkout. This first version intentionally does
   not support the block checkout; block support needs a Store API extension
   rather than DOM hooks.

## Product-readiness checklist

- Replace the development version and author metadata before release.
- Add an uninstall policy and privacy-policy copy if you persist analytics.
- Test against supported WordPress, PHP, WooCommerce, caching, and checkout
  field-editor versions.
- Add block-checkout support before advertising universal WooCommerce
  compatibility.
- Supply end-user documentation, support terms, and a license appropriate for
  your sales channel.

## Security model

The checkout JavaScript only calls a same-origin WordPress REST endpoint with a
WordPress REST nonce. The REST endpoint validates requests, limits query size,
caches successful results for one minute, and keeps the Addresswise key in the
server-side WordPress option.

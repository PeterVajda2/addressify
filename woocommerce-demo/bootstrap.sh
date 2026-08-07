#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

podman-compose up -d db wordpress

for attempt in $(seq 1 30); do
    if curl --silent --fail http://localhost:8088/wp-admin/install.php >/dev/null; then
        break
    fi
    if [[ "$attempt" == 30 ]]; then
        echo "WordPress did not become ready on http://localhost:8088" >&2
        exit 1
    fi
    sleep 2
done

if ! podman-compose run --rm wpcli core is-installed >/dev/null 2>&1; then
    podman-compose run --rm wpcli core install \
        --url=http://localhost:8088 \
        --title="Addresswise WooCommerce Demo" \
        --admin_user=admin \
        --admin_password=local-demo-password \
        --admin_email=admin@example.test \
        --skip-email
fi

if ! podman-compose run --rm wpcli plugin is-installed woocommerce >/dev/null 2>&1; then
    podman-compose run --rm wpcli plugin install woocommerce
fi
podman-compose run --rm wpcli plugin activate woocommerce
podman-compose run --rm wpcli plugin activate addresswise-woocommerce
podman-compose run --rm wpcli eval '
    $checkout = wc_get_page_id( "checkout" );
    $cart = wc_get_page_id( "cart" );
    $shop = wc_get_page_id( "shop" );
    wp_update_post( array( "ID" => $checkout, "post_content" => "[woocommerce_checkout]" ) );
    wp_update_post( array( "ID" => $cart, "post_content" => "[woocommerce_cart]" ) );
    wp_update_post( array( "ID" => $shop, "post_content" => "[products limit=12 columns=4]" ) );
    $product_id = wc_get_product_id_by_sku( "addresswise-demo-mug" );
    if ( ! $product_id ) {
        $product = new WC_Product_Simple();
        $product->set_name( "Addresswise demo mug" );
        $product->set_sku( "addresswise-demo-mug" );
        $product->set_regular_price( "19.90" );
        $product->set_price( "19.90" );
        $product->set_status( "publish" );
        $product->save();
    }
    $settings = (array) get_option( "woocommerce_cod_settings", array() );
    $settings["enabled"] = "yes";
    $settings["title"] = "Cash on delivery";
    update_option( "woocommerce_cod_settings", $settings );
'
podman-compose run --rm wpcli rewrite structure '/%postname%/' --hard
podman-compose run --rm wpcli rewrite flush --hard

echo "Demo store: http://localhost:8088"
echo "Admin: http://localhost:8088/wp-admin (admin / local-demo-password)"

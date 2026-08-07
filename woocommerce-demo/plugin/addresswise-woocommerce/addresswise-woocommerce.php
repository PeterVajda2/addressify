<?php
/**
 * Plugin Name: Addresswise for WooCommerce
 * Description: Addresswise-powered address autocomplete for the classic WooCommerce checkout.
 * Version: 0.1.0
 * Requires at least: 6.4
 * Requires PHP: 8.1
 * Requires Plugins: woocommerce
 * Author: Addresswise
 * License: GPL-2.0-or-later
 * Text Domain: addresswise-woocommerce
 */

defined( 'ABSPATH' ) || exit;

final class Addresswise_WooCommerce {
    const OPTION = 'addresswise_woocommerce_settings';
    const REST_NAMESPACE = 'addresswise-woocommerce/v1';

    public function __construct() {
        add_action( 'plugins_loaded', array( $this, 'load' ) );
    }

    public function load() {
        if ( ! class_exists( 'WooCommerce' ) ) {
            add_action( 'admin_notices', array( $this, 'woocommerce_required_notice' ) );
            return;
        }

        add_action( 'rest_api_init', array( $this, 'register_rest_routes' ) );
        add_action( 'wp_enqueue_scripts', array( $this, 'enqueue_checkout_assets' ) );
        add_action( 'admin_menu', array( $this, 'register_settings_page' ) );
        add_action( 'admin_init', array( $this, 'register_settings' ) );
    }

    public function woocommerce_required_notice() {
        echo '<div class="notice notice-error"><p>' . esc_html__( 'Addresswise for WooCommerce requires WooCommerce to be active.', 'addresswise-woocommerce' ) . '</p></div>';
    }

    public function register_rest_routes() {
        register_rest_route(
            self::REST_NAMESPACE,
            '/search',
            array(
                'methods'             => WP_REST_Server::READABLE,
                'callback'            => array( $this, 'search' ),
                'permission_callback' => array( $this, 'verify_rest_nonce' ),
                'args'                => array(
                    'q'       => array( 'required' => true, 'sanitize_callback' => 'sanitize_text_field' ),
                    'country' => array( 'required' => true, 'sanitize_callback' => 'sanitize_text_field' ),
                ),
            )
        );
    }

    public function verify_rest_nonce( WP_REST_Request $request ) {
        $nonce = $request->get_header( 'X-WP-Nonce' );
        if ( wp_verify_nonce( $nonce, 'wp_rest' ) ) {
            return true;
        }
        return new WP_Error( 'addresswise_invalid_nonce', __( 'Invalid checkout request.', 'addresswise-woocommerce' ), array( 'status' => 403 ) );
    }

    public function search( WP_REST_Request $request ) {
        $settings = $this->settings();
        if ( empty( $settings['enabled'] ) || empty( $settings['api_key'] ) ) {
            return new WP_Error( 'addresswise_not_configured', __( 'Address autocomplete has not been configured yet.', 'addresswise-woocommerce' ), array( 'status' => 503 ) );
        }

        $query = trim( (string) $request['q'] );
        $country = strtoupper( trim( (string) $request['country'] ) );
        if ( '' === $query || strlen( $query ) > 120 || ! preg_match( '/^[A-Z]{2}$/', $country ) ) {
            return new WP_Error( 'addresswise_invalid_request', __( 'Invalid address lookup request.', 'addresswise-woocommerce' ), array( 'status' => 400 ) );
        }

        $cache_key = 'addresswise_' . md5( $country . "\n" . $query );
        $cached = get_transient( $cache_key );
        if ( false !== $cached ) {
            return rest_ensure_response( $cached );
        }

        $endpoint = trailingslashit( esc_url_raw( $settings['api_url'] ) ) . 'search';
        $url = add_query_arg(
            array(
                'q'       => $query,
                'country' => $country,
                'limit'   => 6,
                'api_key' => $settings['api_key'],
            ),
            $endpoint
        );
        $response = wp_remote_get(
            $url,
            array(
                'timeout' => 4,
                'headers' => array( 'Origin' => home_url( '/' ) ),
            )
        );
        if ( is_wp_error( $response ) ) {
            return new WP_Error( 'addresswise_unavailable', __( 'Address autocomplete is temporarily unavailable.', 'addresswise-woocommerce' ), array( 'status' => 502 ) );
        }
        if ( 200 !== wp_remote_retrieve_response_code( $response ) ) {
            return new WP_Error( 'addresswise_lookup_failed', __( 'Address autocomplete could not complete the lookup.', 'addresswise-woocommerce' ), array( 'status' => 502 ) );
        }
        $payload = json_decode( wp_remote_retrieve_body( $response ), true );
        if ( ! is_array( $payload ) || ! isset( $payload['results'] ) || ! is_array( $payload['results'] ) ) {
            return new WP_Error( 'addresswise_invalid_response', __( 'Address autocomplete returned an invalid response.', 'addresswise-woocommerce' ), array( 'status' => 502 ) );
        }
        set_transient( $cache_key, $payload, MINUTE_IN_SECONDS );
        return rest_ensure_response( $payload );
    }

    public function enqueue_checkout_assets() {
        if ( ! is_checkout() || is_order_received_page() || ! $this->settings()['enabled'] ) {
            return;
        }
        wp_enqueue_style( 'addresswise-woocommerce', plugins_url( 'assets/checkout.css', __FILE__ ), array(), '0.1.0' );
        wp_enqueue_script( 'addresswise-woocommerce', plugins_url( 'assets/checkout.js', __FILE__ ), array(), '0.1.0', true );
        wp_localize_script(
            'addresswise-woocommerce',
            'AddresswiseCheckout',
            array(
                'endpoint' => esc_url_raw( rest_url( self::REST_NAMESPACE . '/search' ) ),
                'nonce'    => wp_create_nonce( 'wp_rest' ),
                'minimum'  => 3,
                'messages' => array(
                    'loading' => __( 'Finding addresses…', 'addresswise-woocommerce' ),
                    'empty'   => __( 'No matching addresses found.', 'addresswise-woocommerce' ),
                    'error'   => __( 'Address suggestions are temporarily unavailable.', 'addresswise-woocommerce' ),
                ),
            )
        );
    }

    public function register_settings_page() {
        add_submenu_page( 'woocommerce', __( 'Addresswise', 'addresswise-woocommerce' ), __( 'Addresswise', 'addresswise-woocommerce' ), 'manage_woocommerce', 'addresswise-woocommerce', array( $this, 'render_settings_page' ) );
    }

    public function register_settings() {
        register_setting( 'addresswise_woocommerce', self::OPTION, array( $this, 'sanitize_settings' ) );
    }

    public function sanitize_settings( $input ) {
        return array(
            'enabled' => empty( $input['enabled'] ) ? 0 : 1,
            'api_url' => untrailingslashit( esc_url_raw( $input['api_url'] ?? 'https://addresswise.eu' ) ),
            'api_key' => sanitize_text_field( $input['api_key'] ?? '' ),
        );
    }

    public function render_settings_page() {
        if ( ! current_user_can( 'manage_woocommerce' ) ) {
            return;
        }
        $settings = $this->settings();
        ?>
        <div class="wrap">
            <h1><?php esc_html_e( 'Addresswise for WooCommerce', 'addresswise-woocommerce' ); ?></h1>
            <p><?php esc_html_e( 'The API key stays on your server. Add this store domain to the key in Addresswise before enabling autocomplete.', 'addresswise-woocommerce' ); ?></p>
            <form action="options.php" method="post">
                <?php settings_fields( 'addresswise_woocommerce' ); ?>
                <table class="form-table" role="presentation">
                    <tr><th scope="row"><?php esc_html_e( 'Enable autocomplete', 'addresswise-woocommerce' ); ?></th><td><label><input type="checkbox" name="<?php echo esc_attr( self::OPTION ); ?>[enabled]" value="1" <?php checked( $settings['enabled'] ); ?>> <?php esc_html_e( 'Show address suggestions at checkout', 'addresswise-woocommerce' ); ?></label></td></tr>
                    <tr><th scope="row"><label for="addresswise-api-url"><?php esc_html_e( 'Addresswise API URL', 'addresswise-woocommerce' ); ?></label></th><td><input class="regular-text" id="addresswise-api-url" name="<?php echo esc_attr( self::OPTION ); ?>[api_url]" value="<?php echo esc_attr( $settings['api_url'] ); ?>" type="url" required></td></tr>
                    <tr><th scope="row"><label for="addresswise-api-key"><?php esc_html_e( 'API key', 'addresswise-woocommerce' ); ?></label></th><td><input class="regular-text" id="addresswise-api-key" name="<?php echo esc_attr( self::OPTION ); ?>[api_key]" value="<?php echo esc_attr( $settings['api_key'] ); ?>" type="password" autocomplete="new-password"><p class="description"><?php esc_html_e( 'Use a key authorized for this store domain.', 'addresswise-woocommerce' ); ?></p></td></tr>
                </table>
                <?php submit_button(); ?>
            </form>
        </div>
        <?php
    }

    private function settings() {
        return wp_parse_args(
            get_option( self::OPTION, array() ),
            array(
                'enabled' => 0,
                'api_url' => 'https://addresswise.eu',
                'api_key' => '',
            )
        );
    }
}

new Addresswise_WooCommerce();

(function () {
  const config = window.AddresswiseCheckout;
  if (!config) return;

  const timers = new Map();
  const controllers = new Map();

  function field(prefix, name) {
    return document.getElementById(`${prefix}_${name}`) || document.querySelector(`[name="${prefix}_${name}"]`);
  }

  function hostFor(input) {
    return input.closest('.form-row') || input.parentElement;
  }

  function clear(prefix) {
    const existing = document.getElementById(`addresswise-${prefix}-suggestions`);
    if (existing) existing.remove();
  }

  function value(address, name) {
    return address[name] || '';
  }

  function formatted(address) {
    const house = [value(address, 'premise'), value(address, 'subpremise')].filter(Boolean).join('/');
    return [value(address, 'thoroughfare'), house].filter(Boolean).join(' ') || value(address, 'full_address');
  }

  function render(prefix, input, payload) {
    clear(prefix);
    const wrap = document.createElement('div');
    wrap.id = `addresswise-${prefix}-suggestions`;
    wrap.className = 'addresswise-suggestions';
    const results = payload.results || [];
    if (!results.length) {
      wrap.textContent = config.messages.empty;
    } else {
      const list = document.createElement('ul');
      results.forEach((result) => {
        const button = document.createElement('button');
        button.type = 'button';
        button.textContent = result.formatted;
        button.addEventListener('click', () => {
          const address = result.address;
          input.value = formatted(address);
          const city = field(prefix, 'city');
          const postcode = field(prefix, 'postcode');
          if (city) city.value = value(address, 'locality') || value(address, 'dependent_locality');
          if (postcode) postcode.value = value(address, 'postal_code');
          [input, city, postcode].filter(Boolean).forEach((element) => element.dispatchEvent(new Event('change', { bubbles: true })));
          clear(prefix);
          document.body.dispatchEvent(new Event('update_checkout'));
        });
        const item = document.createElement('li');
        item.appendChild(button);
        list.appendChild(item);
      });
      wrap.appendChild(list);
    }
    hostFor(input).appendChild(wrap);
  }

  async function lookup(prefix, input) {
    const country = field(prefix, 'country');
    const query = input.value.trim();
    if (!country || query.length < config.minimum || !country.value) {
      clear(prefix);
      return;
    }
    if (controllers.has(prefix)) controllers.get(prefix).abort();
    const controller = new AbortController();
    controllers.set(prefix, controller);
    const params = new URLSearchParams({ q: query, country: country.value });
    try {
      const response = await fetch(`${config.endpoint}?${params}`, { headers: { 'X-WP-Nonce': config.nonce }, signal: controller.signal });
      if (!response.ok) throw new Error('Lookup failed');
      render(prefix, input, await response.json());
    } catch (error) {
      if (error.name !== 'AbortError') render(prefix, input, { results: [] });
    }
  }

  document.addEventListener('input', (event) => {
    ['billing', 'shipping'].forEach((prefix) => {
      const input = field(prefix, 'address_1');
      if (!input || event.target !== input) return;
      clearTimeout(timers.get(prefix));
      timers.set(prefix, setTimeout(() => lookup(prefix, input), 180));
    });
  });

  document.addEventListener('change', (event) => {
    ['billing', 'shipping'].forEach((prefix) => {
      if (event.target === field(prefix, 'country')) clear(prefix);
    });
  });
})();

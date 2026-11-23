(() => {
  'use strict';
  const element = id => document.getElementById(id);
  const status = element('account-status');
  const translate = window.AddresswiseI18n.text;
  const language = document.documentElement.lang;
  const plans = ['free', 'starter', 'growth', 'custom'];
  const names = {free: 'Free', starter: 'Starter', growth: 'Growth', custom: 'Custom'};
  let profile;

  async function api(path, body, authenticated = false) {
    const options = {cache: 'no-store'};
    if (body !== undefined) {
      options.method = 'POST';
      options.headers = {'Content-Type': 'application/json'};
      if (authenticated) options.headers['X-CSRF-Token'] = profile.csrf_token;
      options.body = JSON.stringify(body);
    }
    const result = await fetch('/account/api/' + path, options);
    if (result.status === 401 && path === 'me') return null;
    const data = await result.json();
    if (!result.ok) throw new Error(data.message || 'Unable to complete the request. Try again.');
    return data;
  }

  function render() {
    const signedIn = Boolean(profile);
    element('account-entry').hidden = signedIn;
    element('customer-dashboard').hidden = !signedIn;
    element('legacy-account').hidden = signedIn;
    element('credit-usage').hidden = true;
    element('raw-history').hidden = true;
    element('api-key').value = '';
    element('customer-key').value = profile?.api_key || '';
    element('customer-key').type = 'password';
    element('show-key').textContent = translate('Show key');
    if (!profile) return;
    const {customer, subscription, credits} = profile;
    const active = subscription.active_plan;
    const selected = subscription.selected_plan;
    const pending = selected !== 'free' && selected !== active;
    const cancelling = Boolean(subscription.cancel_at);
    element('customer-identity').textContent = customer.name + ' · ' + customer.email;
    element('customer-address').textContent = customer.business_address || '';
    element('customer-address').hidden = !customer.business_address;
    element('customer-domain').textContent = customer.domain;
    element('active-plan').textContent = translate(names[active]);
    element('selected-plan').textContent = translate(names[selected]);
    element('subscription-status').textContent = cancelling ? 'Cancellation scheduled' :
      pending ? 'Awaiting payment' : active === 'free' ? 'Free plan active' : 'Paid plan active';
    element('cancel-effective').hidden = !cancelling;
    element('cancel-at').textContent = subscription.cancel_at || '';
    element('upgrade-form').elements.plan.value = selected === 'free' ? 'starter' : selected;
    element('upgrade-form').hidden = cancelling;
    element('payment-panel').hidden = !pending || cancelling;
    element('cancel-subscription').hidden = cancelling || (active === 'free' && !pending);
    element('cancel-subscription').textContent = active === 'free' ? 'Cancel pending upgrade' : 'Cancel subscription';
    element('key-controls').hidden = !profile.api_key;
    element('customer-key-status').textContent = !profile.api_key ? 'API key unavailable. Contact support.' :
      !profile.key_active ? 'API key disabled. Contact support.' : '';
    if (credits) renderCredits(credits);
  }

  async function load() {
    profile = await api('me');
    render();
  }

  function form(id, handler) {
    const form = element(id);
    const button = form.querySelector('button[type=submit]');
    form.addEventListener('submit', async event => {
      event.preventDefault();
      button.disabled = true;
      status.textContent = '';
      try {
        await handler(Object.fromEntries(new FormData(form)));
        for (const field of form.querySelectorAll('input[type=password]')) field.value = '';
      } catch (error) {
        status.textContent = translate(error.message || 'Unable to complete the request. Try again.');
      } finally {
        button.disabled = false;
      }
    });
    button.disabled = false;
  }

  function openAccount() {
    const url = new URL(location.href);
    url.searchParams.delete('plan');
    url.hash = 'customer-dashboard';
    history.replaceState(null, '', url);
    element('customer-dashboard').scrollIntoView();
  }

  const registration = element('register-form');
  const {company_country: country, ico, name: companyName, vat_id: vat, street, city, postal_code: postal} = registration.elements;
  const lookupStatus = element('registration-lookup-status');
  const addressFields = [street, city, postal];
  const fromRegistry = new Map();
  const results = element('registration-address-results');
  const addressStatus = element('registration-address-status');
  const demo = window.AddresswiseRegistrationDemo;
  let lookupTimer, lookup, addressTimer, addressSearch;
  country.value = language === 'sk' ? 'SK' : 'CZ';

  function resetLookup() {
    clearTimeout(lookupTimer);
    lookup?.abort();
    lookup = undefined;
    lookupStatus.textContent = '';
  }

  function streetLine(name, ...numbers) {
    const house = numbers.filter(number => number && number !== '0').join('/');
    return [name, house].filter(Boolean).join(' ');
  }

  function prefill(field, value) {
    if (field.value || !value || value.length > field.maxLength) return false;
    field.value = value;
    fromRegistry.set(field, value);
    return true;
  }

  async function lookupCompany(id) {
    const controller = new AbortController();
    lookup = controller;
    const timeout = setTimeout(() => controller.abort(), 90000);
    lookupStatus.textContent = translate('Checking company…');
    try {
      const endpoint = '/companywise/api/v1/subjects/' + country.value + '/' + id + '?minimal=true';
      const response = await fetch(endpoint, {signal: controller.signal, cache: 'no-store'});
      if (!response.ok && response.status !== 404) throw new Error('Company lookup failed');
      const data = response.ok ? await response.json() : {};
      if (lookup !== controller) return;
      const record = data.records?.find(entry => entry.name?.trim());
      if (!record) {
        lookupStatus.textContent = translate('No company found. You can enter your details manually.');
        return;
      }
      const changed = [
        prefill(companyName, record.name.trim()),
        prefill(vat, data.vat?.vat_id),
      ];
      const address = record.addresses?.[0];
      if (address && addressFields.every(field => !field.value)) {
        changed.push(
          prefill(street, streetLine(address.street, address.reg_number, address.building_number)),
          prefill(city, address.city || address.municipality),
          prefill(postal, address.postal_codes?.[0]),
        );
      }
      lookupStatus.textContent = translate(changed.some(Boolean)
        ? 'Companywise did the typing. You can edit any field.'
        : 'Companywise found the company.');
    } catch {
      if (lookup === controller) {
        lookupStatus.textContent = translate('Company lookup is unavailable. You can enter your details manually.');
      }
    } finally {
      clearTimeout(timeout);
      if (lookup === controller) lookup = undefined;
    }
  }

  function scheduleCompanyLookup() {
    resetLookup();
    for (const [field, value] of fromRegistry) {
      if (field.value === value) field.value = '';
    }
    fromRegistry.clear();
    const id = ico.value.trim();
    if (/^[0-9]{8}$/.test(id)) lookupTimer = setTimeout(() => lookupCompany(id), 350);
  }

  function cancelAddressSearch() {
    clearTimeout(addressTimer);
    addressSearch?.abort();
    addressSearch = undefined;
    results.replaceChildren();
    addressStatus.textContent = '';
  }

  async function suggestAddress(query) {
    const controller = new AbortController();
    addressSearch = controller;
    const timeout = setTimeout(() => controller.abort(), 15000);
    const streetOnly = !/\d/.test(query);
    const params = new URLSearchParams({q: query, country: country.value, limit: '4', api_key: demo.key});
    try {
      const response = await fetch('/search?' + params + (streetOnly ? '&street_only' : ''), {signal: controller.signal});
      if (!response.ok) throw new Error('Address search failed');
      const data = await response.json();
      if (addressSearch !== controller) return;
      for (const {address} of data.results) {
        if (!address?.full_address) continue;
        const item = document.createElement('li');
        const choice = document.createElement('button');
        choice.type = 'button';
        choice.textContent = address.full_address;
        choice.addEventListener('click', () => {
          cancelAddressSearch();
          if (!address.premise && !address.subpremise) {
            street.value = (address.thoroughfare || address.full_address) + ' ';
            addressStatus.textContent = translate('Add the house number to see complete addresses.');
          } else {
            for (const field of addressFields) fromRegistry.delete(field);
            street.value = streetLine(address.thoroughfare, address.premise, address.subpremise);
            city.value = address.locality || address.dependent_locality || '';
            postal.value = address.postal_code || '';
            addressStatus.textContent = translate('Addresswise did the legwork. You can still edit the address.');
          }
          street.focus();
        });
        item.append(choice);
        results.append(item);
      }
    } catch {
      if (addressSearch === controller) {
        addressStatus.textContent = translate('Address search is unavailable. You can enter the address manually.');
      }
    } finally {
      clearTimeout(timeout);
    }
  }

  ico.addEventListener('input', scheduleCompanyLookup);
  ico.addEventListener('keydown', event => {
    if (event.key === 'Enter') event.preventDefault();
  });
  country.addEventListener('change', () => {
    cancelAddressSearch();
    scheduleCompanyLookup();
  });
  for (const field of [companyName, vat, ...addressFields]) {
    field.addEventListener('input', () => fromRegistry.delete(field));
  }
  street.addEventListener('input', () => {
    cancelAddressSearch();
    const query = street.value.trim();
    if (query.length >= 3 && demo?.key && demo.countries.includes(country.value)) {
      addressTimer = setTimeout(() => suggestAddress(query), 350);
    }
  });
  street.addEventListener('keydown', event => {
    if (event.key === 'Escape') cancelAddressSearch();
    if (event.key === 'Enter' && results.childElementCount) {
      event.preventDefault();
      results.querySelector('button').click();
    }
  });

  form('register-form', async data => {
    resetLookup();
    cancelAddressSearch();
    await api('register', {...data, business: data.business === 'on', accept_terms: data.terms === 'on',
      terms_version: '2026-11-23-accounts', language});
    await load();
    openAccount();
    status.textContent = 'Your account is ready.';
  });

  form('login-form', async data => {
    await api('login', data);
    await load();
    openAccount();
  });
  form('upgrade-form', async data => {
    await api('subscription', data, true);
    await load();
  });
  form('password-form', async data => {
    await api('password', data, true);
    await load();
    status.textContent = 'Password changed.';
  });

  async function action(button, fn) {
    button.disabled = true;
    status.textContent = '';
    try { await fn(); }
    catch (error) { status.textContent = translate(error.message); }
    finally { button.disabled = false; }
  }
  element('cancel-subscription').onclick = event => {
    if (profile.subscription.active_plan !== 'free' &&
        !confirm(translate('Cancel renewal and return to Free at the end of this month?'))) return;
    action(event.currentTarget, async () => {
      await api('subscription/cancel', {}, true);
      await load();
    });
  };
  element('sign-out').onclick = event => action(event.currentTarget, async () => {
    await api('logout', {}, true);
    profile = null;
    render();
    history.replaceState(null, '', location.pathname + location.search);
  });
  element('show-key').onclick = event => {
    const input = element('customer-key');
    const show = input.type === 'password';
    input.type = show ? 'text' : 'password';
    event.currentTarget.textContent = translate(show ? 'Hide key' : 'Show key');
  };
  element('copy-key').onclick = event => action(event.currentTarget, async () => {
    await navigator.clipboard.writeText(profile.api_key);
    status.textContent = 'Copied';
  });

  async function initialize() {
    await load();
    const plan = new URLSearchParams(location.search).get('plan');
    if (profile && plans.includes(plan)) {
      if (plan !== 'free') element('upgrade-form').elements.plan.value = plan;
      openAccount();
    } else if (!profile && location.hash === '#request-access') {
      element('request-access').scrollIntoView();
    }
  }
  initialize().catch(error => { status.textContent = translate(error.message); });
})();

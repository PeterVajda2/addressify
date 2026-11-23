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
    element(id).addEventListener('submit', async event => {
      event.preventDefault();
      const form = event.currentTarget;
      const button = form.querySelector('button[type=submit]');
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
  }

  function openAccount() {
    const url = new URL(location.href);
    url.searchParams.delete('plan');
    url.hash = 'customer-dashboard';
    history.replaceState(null, '', url);
    element('customer-dashboard').scrollIntoView();
  }

  const registration = element('register-form');
  const companyCountry = element('registration-company-country');
  const ico = element('registration-ico');
  const lookupButton = element('registration-company-lookup');
  const lookupStatus = element('registration-lookup-status');
  const companyName = registration.elements.name;
  const businessAddress = registration.elements.business_address;
  let lookup;
  let lastRegistryAddress = '';
  companyCountry.value = language === 'sk' ? 'SK' : 'CZ';

  function resetLookup() {
    lookup?.abort();
    lookup = undefined;
    lookupButton.disabled = false;
    lookupStatus.textContent = '';
  }

  function formatRegistryAddress(address) {
    const numbers = [address.reg_number, address.building_number]
      .filter(number => number && number !== '0');
    const street = [address.street, numbers.join('/')].filter(Boolean).join(' ');
    const locality = [address.postal_codes?.[0], address.municipality || address.city]
      .filter(Boolean).join(' ');
    return street || locality ? [street, locality, address.country].filter(Boolean).join('\n') : '';
  }

  async function lookupCompany() {
    resetLookup();
    const id = ico.value.trim();
    if (!/^[0-9]{8}$/.test(id)) {
      lookupStatus.textContent = translate('Enter an 8-digit IČO.');
      ico.focus();
      return;
    }
    const controller = new AbortController();
    lookup = controller;
    const timeout = setTimeout(() => controller.abort(), 90000);
    const originalName = companyName.value;
    const originalAddress = businessAddress.value;
    lookupButton.disabled = true;
    lookupStatus.textContent = translate('Checking company…');
    try {
      const endpoint = '/companywise/api/v1/subjects/' + companyCountry.value + '/' + id + '?minimal=true';
      const result = await fetch(endpoint, {signal: controller.signal, cache: 'no-store'});
      if (lookup !== controller) return;
      const unavailable = 'Company lookup is unavailable. You can enter your details manually.';
      if (!result.ok && result.status !== 404) throw new Error(unavailable);
      const data = result.ok ? await result.json() : {};
      if (lookup !== controller) return;
      const records = Array.isArray(data.records) ? data.records : [];
      const record = records.find(entry => entry.name?.trim() && entry.name.length <= companyName.maxLength);
      if (!record) {
        lookupStatus.textContent = translate('No company found. You can enter your details manually.');
        return;
      }
      const address = (record.addresses || []).map(formatRegistryAddress)
        .find(value => value && value.length <= businessAddress.maxLength);
      if (companyName.value === originalName) companyName.value = record.name.trim();
      if (businessAddress.value === originalAddress) {
        if (address || (lastRegistryAddress && originalAddress === lastRegistryAddress)) {
          businessAddress.value = address || '';
          lastRegistryAddress = businessAddress.value;
        }
      }
      lookupStatus.textContent = translate(address
        ? 'Companywise just did the typing. You can still edit the name and address.'
        : 'Companywise found the name. You can add the address yourself.');
    } catch {
      if (lookup === controller) {
        lookupStatus.textContent = translate('Company lookup is unavailable. You can enter your details manually.');
      }
    } finally {
      clearTimeout(timeout);
      if (lookup === controller) {
        lookup = undefined;
        lookupButton.disabled = false;
      }
    }
  }

  ico.addEventListener('input', resetLookup);
  companyCountry.addEventListener('change', resetLookup);
  lookupButton.addEventListener('click', lookupCompany);
  ico.addEventListener('keydown', event => {
    if (event.key === 'Enter') {
      event.preventDefault();
      if (!lookupButton.disabled) lookupCompany();
    }
  });

  const addressHelper = element('registration-address-helper');
  const addressCountry = element('registration-address-country');
  const addressQuery = element('registration-address-query');
  const addressResults = element('registration-address-results');
  const addressStatus = element('registration-address-status');
  const addressNote = element('registration-address-note');
  const demo = window.AddresswiseRegistrationDemo;
  let addressTimer, addressSearch;

  function cancelAddressSearch() {
    clearTimeout(addressTimer);
    addressSearch?.abort();
    addressSearch = undefined;
    addressResults.replaceChildren();
    addressStatus.textContent = '';
  }

  async function suggestAddress(query) {
    const controller = new AbortController();
    addressSearch = controller;
    const timeout = setTimeout(() => controller.abort(), 15000);
    const streetOnly = !/\d/.test(query);
    const params = new URLSearchParams({q: query, country: addressCountry.value, limit: '4', api_key: demo.key});
    addressStatus.textContent = translate('Finding addresses…');
    try {
      const response = await fetch('/search?' + params + (streetOnly ? '&street_only' : ''), {signal: controller.signal});
      if (!response.ok) throw new Error('Address search failed');
      const data = await response.json();
      if (addressSearch !== controller) return;
      for (const {address} of data.results) {
        if (!address?.full_address || address.full_address.length > businessAddress.maxLength) continue;
        const item = document.createElement('li');
        const choice = document.createElement('button');
        choice.type = 'button';
        choice.textContent = address.full_address;
        choice.addEventListener('click', () => {
          cancelAddressSearch();
          if (streetOnly && !address.premise && !address.subpremise) {
            addressQuery.value = (address.thoroughfare || address.full_address) + ' ';
            addressStatus.textContent = translate('Add the house number to see complete addresses.');
            addressQuery.focus();
            return;
          }
          lastRegistryAddress = '';
          businessAddress.value = address.full_address;
          addressQuery.value = '';
          addressHelper.open = false;
          addressNote.textContent = translate('Addresswise did the legwork. You can still edit the address.');
          businessAddress.focus();
        });
        item.append(choice);
        addressResults.append(item);
      }
      addressStatus.textContent = translate(addressResults.childElementCount
        ? streetOnly ? 'Select a street, then add the house number.' : 'Select an address.'
        : 'No addresses found. Try a street and city.');
    } catch {
      if (addressSearch === controller) {
        addressStatus.textContent = translate('Address search is unavailable. You can enter the address manually.');
      }
    } finally {
      clearTimeout(timeout);
    }
  }

  if (demo?.key && demo.countries.length) {
    const regionNames = new Intl.DisplayNames([language], {type: 'region'});
    const countries = demo.countries.map(code => [code, regionNames.of(code) || code]);
    countries.sort((a, b) => a[1].localeCompare(b[1], language));
    for (const [code, name] of countries) addressCountry.add(new Option(name, code));
    if (demo.countries.includes(companyCountry.value)) addressCountry.value = companyCountry.value;
    addressHelper.hidden = false;
    const schedule = () => {
      cancelAddressSearch();
      const query = addressQuery.value.trim();
      if (query.length >= 3) addressTimer = setTimeout(() => suggestAddress(query), 350);
      else if (query) addressStatus.textContent = translate('Type at least 3 characters.');
    };
    addressQuery.addEventListener('input', schedule);
    addressCountry.addEventListener('change', schedule);
    addressQuery.addEventListener('keydown', event => {
      if (event.key === 'Enter') event.preventDefault();
    });
    addressHelper.addEventListener('toggle', () => {
      if (!addressHelper.open) cancelAddressSearch();
    });
    businessAddress.addEventListener('input', () => { addressNote.textContent = ''; });
  }

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

/* Website localization only. API contracts, examples and source records stay unchanged. */
(() => {
  'use strict';
  const language=document.documentElement.lang;
  const words=window.AddresswiseMessages||{};
  const formats={cs:{credits:'kreditů',month:'měsíc',perMonth:'kreditů/měsíc',fits:'pokryje tento odhad.',reserved:'kreditů je rezervováno probíhajícími požadavky.',hard:'Pevný limit: požadavky nad limit se zablokují s HTTP 429 / credit_limit_exceeded. Obnova:',started:'Kreditové měření začalo',earlier:'Původní společné počty dotazů se zpětně nepřevádějí ani neúčtují.',established:'Vznik',confidence:'shoda'},sk:{credits:'kreditov',month:'mesiac',perMonth:'kreditov/mesiac',fits:'pokryje tento odhad.',reserved:'kreditov je rezervovaných prebiehajúcimi požiadavkami.',hard:'Pevný limit: požiadavky nad limit sa zablokujú s HTTP 429 / credit_limit_exceeded. Obnova:',started:'Kreditové meranie začalo',earlier:'Pôvodné spoločné počty dopytov sa spätne neprenášajú ani neúčtujú.',established:'Vznik',confidence:'zhoda'}};
  function text(value) {
    if(language==='en')return value;
    if(words[value])return words[value];
    const f=formats[language];let m;
    if((m=value.match(/^(.*?) \(€(.*?)\/month\) fits this estimate\. Your chosen package never upgrades automatically\.$/s)))return `${text(m[1])} (${m[2]} €/${f.month}) ${f.fits} ${text('Your chosen package never upgrades automatically.')}`;
    if((m=value.match(/^(.*?) credits are reserved by requests currently in progress\.$/)))return `${m[1]} ${f.reserved}`;
    if(value.startsWith('Hard limit: requests that would exceed this package'))return f.hard+' '+value.split('Reset: ')[1];
    if((m=value.match(/^Credit metering started (.*?)\. Earlier combined request counts are not retroactively converted or charged\.$/)))return `${f.started} ${m[1]}. ${f.earlier}`;
    if((m=value.match(/^(.*?) · (.*?) credits\/month · (.*)$/)))return `${text(m[1])} · ${m[2]} ${f.perMonth} · ${text(m[3]).replace('/month','/'+f.month)}`;
    if((m=value.match(/^Unlimited · No monthly credit limit · €(\d+)\/month$/)))return `Unlimited · ${text('No monthly credit limit')} · ${m[1]} €/${f.month}`;
    if((m=value.match(/^Established (.*)$/)))return f.established+' '+m[1];
    if((m=value.match(/^(✓ Validated|Needs review) · (.*?)% confidence$/)))return text(m[1])+` · ${m[2]}% ${f.confidence}`;
    if((m=value.match(/^\s*(.*?)% confidence\.(.*)$/)))return ` ${m[1]}% ${f.confidence}.${text(m[2])}`;
    if((m=value.match(/^(Search|Validation) failed with HTTP (\d+)$/)))return (language==='cs'?'Požadavek selhal s HTTP ':'Požiadavka zlyhala s HTTP ')+m[2];
    if((m=value.match(/^Request failed \((\d+)\)$/)))return (language==='cs'?'Požadavek selhal':'Požiadavka zlyhala')+` (${m[1]})`;
    if(value.includes('legacy address requests'))return value.replace('legacy address requests',language==='cs'?'původních adresních požadavků':'pôvodných adresných požiadaviek').replace(' package','').replace('credits/month',f.perMonth).replace('created ',language==='cs'?'vytvořeno ':'vytvorené ').replace('last used ',language==='cs'?'naposledy použito ':'naposledy použité ').replace(' from ',language==='cs'?' z ':' z ');
    if((m=value.match(/^Existing key ID: (.*)$/)))return (language==='cs'?'ID existujícího klíče: ':'ID existujúceho kľúča: ')+m[1];
    if(value.includes(' · pending · '))return value.replace(' · pending · ',language==='cs'?' · čeká · ':' · čaká · ');
    if(value.includes(' · handled · '))return value.replace(' · handled · ',language==='cs'?' · vyřízeno · ':' · vybavené · ');
    if(value.endsWith(' (deleted key)'))return value.replace(' (deleted key)',language==='cs'?' (smazaný klíč)':' (vymazaný kľúč)');
    if(value.endsWith(' (public demo)'))return value.replace(' (public demo)',language==='cs'?' (veřejná ukázka)':' (verejná ukážka)');
    return value;
  }
  window.AddresswiseI18n={language,text};
  function links(root){for(const link of root.querySelectorAll?.('a[href]')||[]){const raw=link.getAttribute('href');if(!raw||raw.startsWith('#')||/^(mailto:|tel:)/.test(raw))continue;const target=new URL(raw,location.href);if(target.origin!==location.origin||/\/(api|assets|docs-assets)\//.test(target.pathname)||['/search','/suggest','/validate','/health'].includes(target.pathname))continue;target.searchParams.set('lang',language);link.setAttribute('href',target.pathname+target.search+target.hash);}}
  const protectedSelector='script,style,pre,code,[data-source],.suggestion,#address-selected,#company-result,#validation-result li';
  function apply(root){
    if(root.nodeType===Node.TEXT_NODE){const parent=root.parentElement;if(!parent||parent.closest(protectedSelector))return;const value=root.nodeValue.trim();const next=text(value);if(next!==value)root.nodeValue=root.nodeValue.replace(value,next);return;}
    if(root.nodeType!==Node.ELEMENT_NODE||root.closest(protectedSelector))return;
    for(const attr of ['placeholder','aria-label','title']){if(root.hasAttribute(attr)){const value=root.getAttribute(attr);const next=text(value);if(next!==value)root.setAttribute(attr,next);}}
    for(const child of root.childNodes)apply(child);
  }
  function save(value){document.cookie='addresswise_language='+value+'; Path=/; Max-Age=31536000; SameSite=Lax'+(location.protocol==='https:'?'; Secure':'');}
  document.addEventListener('DOMContentLoaded',()=>{
    const selector=document.getElementById('language-selector');if(selector){selector.value=language;selector.addEventListener('change',()=>{save(selector.value);const url=new URL(location.href);url.searchParams.set('lang',selector.value);location.assign(url.pathname+url.search+url.hash);});}
    if(new URL(location.href).searchParams.has('lang'))save(language);
    apply(document.body);links(document);
    const observer=new MutationObserver(records=>{observer.disconnect();for(const record of records){if(record.type==='characterData')apply(record.target);else if(record.type==='attributes')apply(record.target);else for(const node of record.addedNodes){apply(node);if(node.nodeType===Node.ELEMENT_NODE)links(node);}}observe();});
    function observe(){observer.observe(document.body,{subtree:true,childList:true,characterData:true,attributes:true,attributeFilter:['placeholder','aria-label','title']});}observe();
  });
})();

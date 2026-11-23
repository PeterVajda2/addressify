#!/usr/bin/env python3
"""Build reviewed CS/SK HTML from English source without changing API/code data."""
from pathlib import Path
from html import escape
from html.parser import HTMLParser
import json,re
ROOT=Path(__file__).resolve().parents[1]
PAGES=('platform','docs','account','index','admin','terms')
CATALOGS={lang:json.loads((ROOT/'static/locales'/f'{lang}.json').read_text()) for lang in ('cs','sk')}
SELECTOR='<label class="language-control" for="language-selector"><span>Language</span><select id="language-selector" aria-label="Language"><option value="cs">CZ</option><option value="sk">SK</option><option value="en">EN</option></select></label>'
STYLES='''<style>.language-control{display:flex;align-items:center;gap:8px;margin:0;font-size:13px;font-weight:600;white-space:nowrap}.language-control>span{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0)}.language-control select{width:73px;min-height:40px;padding:8px;border:1px solid #9dabbf;border-radius:7px;background-color:white;color:#162239;font:600 13px system-ui}.locale-toolbar{display:flex;justify-content:flex-end;margin-bottom:16px}.main-nav>.links{flex:1;justify-content:flex-end}.business-note{font-size:14px;color:#536177;margin-top:18px}footer .terms-link{white-space:nowrap}@media(max-width:740px){.main-nav>.brand{order:1}.main-nav>.language-control{order:2;margin-left:auto}.main-nav>.links{order:3;flex:1 0 100%;gap:12px;justify-content:space-between}.custom-price{white-space:normal;font-size:26px;line-height:1.15}}@media print{.docs-layout{display:block}.wrap{width:100%}nav,.language-control,.docs-sidebar,.skip-link{display:none!important}.section{break-inside:avoid}body{font-size:11pt}}</style>'''
class Localize(HTMLParser):
 def __init__(self,lang):super().__init__(convert_charrefs=False);self.lang=lang;self.words=CATALOGS.get(lang,{});self.out=[];self.stack=[]
 def t(self,s):
  x=s.strip();return s.replace(x,self.words.get(x,x),1) if x else s
 def handle_starttag(self,tag,attrs):
  protected=any(x in ('script','style','pre','code') for x in self.stack)
  if tag not in ('area','base','br','col','embed','hr','img','input','link','meta','param','source','track','wbr'):self.stack.append(tag)
  values=[]
  for key,value in attrs:
   if value is None:values.append(key);continue
   if not protected and (key in ('aria-label','placeholder','title','alt') or (tag=='meta' and key=='content' and ('name','description') in attrs)):value=self.t(value)
   if tag=='html' and key=='lang':value=self.lang
   if tag=='a' and key=='href' and value.startswith('/') and not value.startswith('//'):
    from urllib.parse import urlsplit,urlunsplit,parse_qsl,urlencode
    parts=urlsplit(value)
    if not parts.path.startswith(('/assets/','/companywise/api/')):
     query=dict(parse_qsl(parts.query,keep_blank_values=True));query['lang']=self.lang;value=urlunsplit(('', '',parts.path,urlencode(query),parts.fragment))
   values.append(key+'="'+escape(value,quote=True)+'"')
  if tag=='option' and ('value',self.lang) in attrs and 'select' in self.stack and self.stack[-2:] == ['select','option']:
   # Select only the language control, never the country or package selectors.
   pass
  self.out.append('<'+tag+(' '+' '.join(values) if values else '')+'>')
 def handle_endtag(self,tag):
  self.out.append('</'+tag+'>')
  if tag in self.stack:
   while self.stack:
    if self.stack.pop()==tag:break
 def handle_data(self,data):self.out.append(data if any(x in ('script','style','pre','code') for x in self.stack) else self.t(data))
 def handle_entityref(self,name):self.out.append('&'+name+';')
 def handle_charref(self,name):self.out.append('&#'+name+';')
 def handle_decl(self,d):self.out.append('<!'+d+'>')
 def handle_comment(self,d):self.out.append('<!--'+d+'-->')
for page in PAGES:
 path=ROOT/'static'/f'{page}.html'
 if not path.exists():continue
 source=path.read_text()
 if '<body' not in source:source=source.replace('</head>','</head><body>',1)
 # Only generated files contain the shared toolbar and scripts.
 if page in ('platform','docs','account','terms'):source=source.replace('</div></nav>','</div>'+SELECTOR+'</nav>',1)
 else:source=source.replace('<main>','<main><div class="locale-toolbar">'+SELECTOR+'</div>',1)
 source=source.replace('</footer>',' <a class="terms-link" href="/terms">Terms of service</a></footer>',1)
 if '</footer>' not in source:source=source.replace('</main>','<p><a href="/terms">Terms of service</a></p></main>',1)
 source=source.replace('</head>',STYLES+'<script>window.AddresswiseMessages=__UI_MESSAGES__;</script><script src="/assets/addresswise-i18n.js"></script></head>',1)
 source=source.replace('new Intl.DisplayNames(["en"]','new Intl.DisplayNames([document.documentElement.lang]')
 source=source.replace('.toLocaleString()', '.toLocaleString(document.documentElement.lang)')
 # Localize explicit labels rendered next to source records before skipping that subtree.
 source=source.replace('line(companyResult,vatText+', 'line(companyResult,window.AddresswiseI18n.text(vatText)+')
 source=source.replace('line(companyResult,"Established "+dates[0])','line(companyResult,window.AddresswiseI18n.text("Established "+dates[0]))')
 for lang in ('en','cs','sk'):
  parser=Localize(lang);parser.feed(source);html=''.join(parser.out)
  # HTMLParser entity references split text nodes: translate those combined phrases too.
  if lang!='en':
   for en,translated in sorted(CATALOGS[lang].items(),key=lambda item:-len(item[0])):
    encoded=escape(en,quote=False)
    if '&amp;' in encoded:html=html.replace(encoded,escape(translated,quote=False))
  if lang=='cs':html=html.replace('<p>Použití <code>CZ</code>', '<p>Použijte <code>CZ</code>').replace('<p>Použití <a ', '<p>Použijte <a ')
  if lang=='sk':html=html.replace('<p>Použitie <code>CZ</code>', '<p>Použite <code>CZ</code>').replace('<p>Použitie <a ', '<p>Použite <a ')
  html=html.replace('__UI_MESSAGES__' ,json.dumps(CATALOGS.get(lang,{}),ensure_ascii=False,separators=(',',':')).replace('<','\\u003c'))
  html=html.replace('<option value="'+lang+'">'+{'cs':'CZ','sk':'SK','en':'EN'}[lang]+'</option>','<option value="'+lang+'" selected>'+{'cs':'CZ','sk':'SK','en':'EN'}[lang]+'</option>')
  dest=ROOT/'static'/lang;dest.mkdir(exist_ok=True);(dest/f'{page}.html').write_text(html)
print('Built CS/SK/EN pages with unchanged API examples.')

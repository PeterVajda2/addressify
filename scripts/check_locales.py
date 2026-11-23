#!/usr/bin/env python3
"""Check translated page structure and invariant API examples."""
from pathlib import Path
from html.parser import HTMLParser
import json
ROOT=Path(__file__).resolve().parents[1]
class Page(HTMLParser):
 def __init__(self):super().__init__();self.stack=[];self.pre=[];self.text=[];self.ids=[]
 def handle_starttag(self,t,a):
  if t=='pre':self.pre.append('')
  if t not in ['meta','link','input','br','hr','img']:self.stack.append(t)
  self.ids.extend(v for k,v in a if k=='id')
 def handle_endtag(self,t):
  if t in self.stack:
   while self.stack:
    if self.stack.pop()==t:break
 def handle_data(self,s):
  if 'pre' in self.stack:self.pre[-1]+=s
  if not any(x in self.stack for x in ['script','style','code','pre']):self.text.append(s)
words={lang:json.loads((ROOT/'static/locales'/f'{lang}.json').read_text()) for lang in ['cs','sk']}
assert set(words['cs'])==set(words['sk'])
for name in ['platform','docs','account','index','admin','terms']:
 pages={}
 for lang in ['en','cs','sk']:
  html=(ROOT/'static'/lang/f'{name}.html').read_text();page=Page();page.feed(html);pages[lang]=page
  assert f'<html lang="{lang}">' in html
  assert page.ids.count('language-selector')==1
  assert len(page.ids)==len(set(page.ids)),(name,lang,'duplicate IDs')
  assert '/terms?lang='+lang in html
  if name=='terms':
   assert '23369817' in html and 'peter@addresswise.eu' in html
   assert all(f'terms-{i}' in page.ids for i in range(1,13))
 for lang in ['cs','sk']:assert pages[lang].pre==pages['en'].pre,(name,lang,'API examples changed')
print('PASS: complete CS/SK catalogs, all six pages and terms variants, unique selectors/IDs, invariant API/JSON examples')

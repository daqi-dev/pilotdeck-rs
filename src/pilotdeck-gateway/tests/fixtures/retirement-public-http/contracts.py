"""Exact retained public HTTP business contracts; no process or score shortcuts."""
import base64,datetime,hashlib,json,re,io
from collections import Counter
from http.client import HTTPResponse
from urllib.parse import urlsplit
from pathlib import Path

OPERATIONS={'list_general_skills','list_model_catalog','model_prepare','model_stream','file_parse','read_sop_authority'}
CASES=[('describe-configured',200),('skills-success',200),('catalog-success',200),('model-prepare-success',200),('model-stream-success',200),('model-prepare-missing-id',400),('model-prepare-unknown-model',400),('model-prepare-invalid-messages',400),('model-prepare-budget',400),('model-stream-invalid-messages',400),('file-text-success',200),('file-markdown-success',200),('file-invalid-base64',400),('file-unsupported',415),('file-too-large',413),('file-invalid-utf8',422),('file-invalid-pdf',422),('sop-authority-handoff',403),('sop-authority-success',200),('sop-authority-invalid',403),('sop-authority-revision',409),('sop-authority-admission',403),('sop-authority-foreign-session',403),('scope-override',400),('unadvertised-tool',409),('describe-missing-principal',400),('describe-get',405),('describe-unauthorized',401)]
APPROVALS=[('status-configured',200),('resume-missing-auth',401),('status-foreign-actor',403),('status-configured',200),('resume-configured',200),('resume-replay',200),('status-configured',200)]
PRINCIPAL={'pilotDeckUserId':'pilot-owner','tenantId':'tenant','actorUserId':'actor','agentId':'target'}
class ContractError(ValueError):pass
def need(ok,rule):
 if not ok:raise ContractError(rule)
def pinned_bytes(path,expected):
 need(isinstance(expected,str) and re.fullmatch(r'[0-9a-f]{64}',expected) is not None,'SOURCE_SHA_FORMAT')
 data=Path(path).read_bytes();need(hashlib.sha256(data).hexdigest()==expected,'SOURCE_OR_RAW_SHA_CHANGED');return data
def canonical(value):return json.dumps(value,sort_keys=True,separators=(',',':'),ensure_ascii=False)
def decode(row):
 raw=bytes.fromhex(row['responseBodyHex']);text=raw.decode('utf-8');need(text==row['body'],'HTTP_HEX_BODY')
 need(type(row['status']) is int and row['headers'].get('content-type','').split(';')[0] in ['application/json','application/x-ndjson'],'HTTP_RESPONSE_HEADERS')
 if row['path'].endswith('/call') and row.get('label')=='model-stream-success':
  value=[json.loads(line) for line in text.splitlines() if line];need(bool(value),'MODEL_NDJSON_EMPTY');return value
 return json.loads(text)
def request(row):
 value=row['requestBody'];return json.loads(value) if isinstance(value,str) else value
def cursor(value,project):
 decoded=json.loads(base64.urlsafe_b64decode(value+'==='));need(set(decoded)=={'offset','signature'} and decoded['offset']==10,'SKILLS_CURSOR')
 signature=json.loads(base64.urlsafe_b64decode(decoded['signature']+'==='));need(signature=={'projectKey':project,'query':'','scope':'all'},'SKILLS_CURSOR_SIGNATURE')
 return {'offset':10,'signature':{'projectKey':'$CURRENT_PROJECT','query':'','scope':'all'}}
def skill(item,context):
 slug=item['slug'];root=Path(context['skills']);display=Path(context.get('skillsDisplay',context['skills']));need(re.fullmatch(r'[A-Za-z0-9_-]+',slug) is not None,'SKILL_SLUG')
 file=root/slug/'SKILL.md';need(item['skillFile']==str(display/slug/'SKILL.md') and item['skillDir']==str(display/slug),'SKILL_SOURCE_PATH')
 rel=slug+'/SKILL.md';need(hashlib.sha256(file.read_bytes()).hexdigest()==context['skillHashes'][rel],'SKILL_SOURCE_BYTES')
 need(type(item['mtime']) in [int,float] and abs(item['mtime']-file.stat().st_mtime_ns/1000000)<=.001,'SKILL_SOURCE_MTIME')
 result=dict(item);result.update(skillFile='$VERIFIED_SKILLS/'+rel,skillDir='$VERIFIED_SKILLS/'+slug,mtime='$VERIFIED_ASSET_MTIME');return result
def normalized(label,value,context):
 # Formal results stay byte-for-byte JSON values. Only explicitly diagnostic replay transforms environment locations.
 original=value;value=json.loads(json.dumps(value));project=context['project'];origin=context['mockOrigin']
 if label=='skills-success':
  need(value['projectPath']==project,'SKILLS_PROJECT_PATH');value['projectPath']='$CURRENT_PROJECT'
  for key in ['data','items','builtin']:
   value[key]=[skill(item,context) for item in value[key]]
  for key in ['nextCursor','next_cursor']:value[key]=cursor(value[key],project)
 elif label=='sop-authority-success':
  need(value['projectKey']==project and value['admission']['projectKey']==project and value['admission']['staffDeckOrigin']==origin,'SOP_CURRENT_ENV')
  value['projectKey']=value['admission']['projectKey']='$CURRENT_PROJECT';value['admission']['staffDeckOrigin']='$CURRENT_MOCK_ORIGIN'
 elif label=='model-stream-success':
  need(value[0]['providerBaseUrl']==origin+'/v1','MODEL_CURRENT_ORIGIN');value[0]['providerBaseUrl']='$CURRENT_MOCK_ORIGIN/v1'
 elif label=='status-configured' and value['status'].get('wait'):
  stamp=value['status']['wait']['createdAt'];need(re.fullmatch(r'\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z',stamp) is not None,'WAIT_TIMESTAMP_FORMAT')
  milliseconds=datetime.datetime.fromisoformat(stamp.replace('Z','+00:00')).timestamp()*1000
  if context.get('seedCreatedAt'):need(stamp==context['seedCreatedAt'],'SEEDED_WAIT_TIMESTAMP')
  elif context.get('processBounds'):need(context['processBounds'][0]<=milliseconds<=context['processBounds'][1],'WAIT_TIMESTAMP_PROCESS_BOUND')
  value['status']['wait']['createdAt']='$VALIDATED_CURRENT_WAIT_CREATION'
 return value if context.get('diagnosticEnvironmentMapping') else json.loads(json.dumps(original))

def module_contract(raw,profile,context,models):
 if profile=='skills-disabled':
  rows=raw['rows'];need(len(rows)==2 and [r['path'] for r in rows]==['/api/module-host/describe','/api/module-host/call'],'DISABLED_CASES')
  values=[decode(r) for r in rows];need([r['status'] for r in rows]==[200,409] and set(values[0]['operations'])==OPERATIONS-{'list_general_skills'} and values[1]['code']=='PILOTDECK_HOST_CAPABILITY_UNAVAILABLE','DISABLED_CAPABILITY')
  return {'rows':values}
 if profile=='approval-unconfigured':
  rows=raw['rows'];need(len(rows)==2 and [r['path'] for r in rows]==['/api/module-host/approvals/status','/api/module-host/approvals/resume'],'UNCONFIGURED_CASES')
  values=[decode(r) for r in rows];need([r['status'] for r in rows]==[403,403] and all(v['code']=='APPROVAL_SUBJECT_MISMATCH' for v in values),'UNCONFIGURED_REFUSAL')
  return {'rows':values}
 rows=raw['hostRows'];approvals=raw['approvalRows']
 need([(r['label'],r['status']) for r in rows]==CASES and [(r['label'],r['status']) for r in approvals]==APPROVALS,'CONFIGURED_CASES_EXACT')
 values={r['label']:decode(r) for r in rows};av=[decode(r) for r in approvals]
 need(len(values)==28 and set(values['describe-configured']['operations'])==OPERATIONS and len(values['describe-configured']['operations'])==6,'SIX_CAPABILITIES')
 for r in rows:
  label=r['label'];expected='/api/module-host/describe' if label.startswith('describe-') else '/api/module-host/call'
  need(r['path']==expected and r.get('method','POST')==('GET' if label=='describe-get' else 'POST'),'EXACT_HTTP_METHOD_PATH')
  body=request(r)
  if label not in ['describe-missing-principal','describe-get']:need(body.get('principal')==PRINCIPAL,'EXACT_PRINCIPAL')
  if expected.endswith('/call'):
   operation=('list_general_skills' if label in ['skills-success','scope-override'] else 'list_model_catalog' if label=='catalog-success' else 'model_prepare' if label.startswith('model-prepare-') else 'model_stream' if label.startswith('model-stream-') else 'file_parse' if label.startswith('file-') else 'read_sop_authority' if label.startswith('sop-') else 'list_tools')
   need(body.get('operation')==operation,'EXACT_CASE_OPERATION')
 model={'requestId':'host-model-42','modelId':'fixture/mock','request':{'messages':[{'role':'user','content':[{'type':'text','text':'HOST_MODEL_42'}]}],'maxOutputTokens':32},'budget':{'maxOutputTokens':32,'timeoutMs':5000}}
 inactive={'sessionKey':'web:sop-fixture','projectKey':context['project'],'requestId':'host-sop-inactive-42','expectedRevision':5,'admissionCredentialId':'credential'}
 active={**inactive,'requestId':'host-sop-42','expectedRevision':6}
 expected_inputs={'skills-success':{},'catalog-success':{},'model-prepare-success':model,'model-stream-success':model,'model-prepare-missing-id':{k:v for k,v in model.items() if k!='modelId'},'model-prepare-unknown-model':{**model,'modelId':'fixture/absent'},'model-prepare-invalid-messages':{**model,'request':{'messages':[]}},'model-prepare-budget':{**model,'budget':{'maxOutputTokens':0}},'model-stream-invalid-messages':{**model,'request':{'messages':[]}},'file-text-success':{'filename':'entry.txt','content_base64':'SE9TVF9URVhUXzQyCg=='},'file-markdown-success':{'filename':'entry.md','media_type':'text/markdown','content_base64':'IyBIT1NUX01BUktET1dOXzQyCg=='},'file-invalid-base64':{'filename':'entry.txt','content_base64':'$$'},'file-unsupported':{'filename':'entry.bin','content_base64':'NDI='},'file-too-large':{'filename':'entry.txt','content_base64':'NDI=','max_bytes':1},'file-invalid-utf8':{'filename':'entry.txt','content_base64':'/w=='},'file-invalid-pdf':{'filename':'entry.pdf','content_base64':'NDI='},'sop-authority-handoff':inactive,'sop-authority-success':active,'sop-authority-invalid':{},'sop-authority-revision':{**active,'expectedRevision':7},'sop-authority-admission':{**active,'admissionCredentialId':'foreign'},'sop-authority-foreign-session':{**active,'sessionKey':'web:missing'},'scope-override':{'tenantId':'foreign'},'unadvertised-tool':{}}
 for row in rows:
  if row['label'] in expected_inputs:need(canonical(request(row)['input'])==canonical(expected_inputs[row['label']]),'EXACT_CASE_INPUT')
 base={'sessionKey':'web:sop-fixture','projectKey':context['project'],'tenantId':'tenant','agentId':'target'}
 resume={**base,'source':'human','requestId':'human-reply-42','waitId':'human-wait-42','expectedRevision':5,'message':'SOP_APPROVAL_DECISION_42','slotUpdates':{'decision':'SOP_APPROVAL_DECISION_42'}}
 for row in approvals:
  need(request(row)==(base if row['path'].endswith('/status') else resume),'EXACT_APPROVAL_INPUT')
 skills=values['skills-success'];need(len(skills['data'])==len(skills['items'])==10 and len(skills['builtin'])==32 and skills['user']==[] and skills['project']==[] and skills['items']==skills['data'] and skills['nextCursor']==skills['next_cursor'],'SKILLS_DEFAULT_SHAPE')
 expected={p.split('/')[0] for p in context['skillHashes'] if p.count('/')==1 and p.endswith('/SKILL.md')}
 need(len(expected)==32 and {x['slug'] for x in skills['builtin']}==expected,'SKILLS_DEFAULT_EXACT_NAMES')
 cursor(skills['nextCursor'],context['project'])
 need(values['file-text-success']['text']=='HOST_TEXT_42\n' and values['file-markdown-success']['text']=='# HOST_MARKDOWN_42\n','FILE_PARSE_CONTENT')
 stream=values['model-stream-success'];need([x['type'] for x in stream]==['request_started','message_start','text_delta','usage','message_end'],'MODEL_NDJSON_SEQUENCE')
 need(stream[2]['text']=='BUSINESS_DENY_DONE' and stream[3]['usage']=={'inputTokens':11000,'outputTokens':5,'totalTokens':11005},'MODEL_NDJSON_CONTENT')
 actual=[m for m in models if m.get('stream') is True and 'HOST_MODEL_42' in canonical(m.get('messages',[]))];need(len(actual)==1 and actual[0]['model']=='mock','ACTUAL_PUBLIC_MODEL_REQUEST')
 need(av[0]['status']['revision']==av[3]['status']['revision']==5 and av[0]['status']['state']['status']=='handoff' and av[0]['status']['approval']['assigneeUserId']=='approver','PENDING_APPROVAL')
 need(av[1]['code']=='APPROVAL_AUTH_REQUIRED' and av[2]['code']=='APPROVAL_SUBJECT_MISMATCH','APPROVAL_AUTHORITY')
 need(av[4]=={'accepted':True,'duplicate':False,'sessionId':'web:sop-fixture','requestId':'human-reply-42','revision':6,'message':'SOP_APPROVAL_DECISION_42'} and av[5]=={**av[4],'duplicate':True},'APPROVAL_DUPLICATE_RECEIPT')
 need(av[6]['status']['revision']==6 and av[6]['status']['state']['status']=='active' and av[6]['status']['state']['slots_json']=={'decision':'SOP_APPROVAL_DECISION_42'},'APPROVAL_DURABLE_REVISION')
 durable=context['durable'];need(durable['schemaVersion']==3 and durable['sessionId']=='web:sop-fixture' and durable['revision']==6 and durable['state']==av[6]['status']['state'] and set(durable['resumeRequests'])=={'human-reply-42'},'SOP_POST_REAP_STATE')
 receipt=durable['resumeRequests']['human-reply-42'];need(all(receipt[k]==v for k,v in av[4].items()) and receipt['waitId']=='human-wait-42' and receipt['source']=='human' and receipt['slotUpdates']=={'decision':'SOP_APPROVAL_DECISION_42'} and receipt['authorization']['expectedRevision']==5 and receipt['authorization']['authority']['subject']['userId']=='approver','SOP_POST_REAP_RECEIPT')
 authority=values['sop-authority-success'];need(authority['sopId']=='fixture' and authority['expectedRevision']==6 and authority['admission']['credentialId']=='credential','SOP_AUTHORITY_ADMISSION')
 return {'hostRows':[(r['label'],r['status'],normalized(r['label'],values[r['label']],context)) for r in rows], 'approvalRows':[(r['label'],r['status'],normalized(r['label'],v,context)) for r,v in zip(approvals,av)]}

def feishu_contract(raw,cases):
 need(raw['channel']=='feishu' and raw['fixedTS']=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85' and raw['failure'] is None and raw['exit']=={'code':0,'signal':None},'FEISHU_EXECUTION')
 callbacks=[s for s in raw['steps'] if s.get('event')=='configured-http-webhook'];need(len(callbacks)==3,'FEISHU_CALLBACK_COUNT')
 for row,expected in zip(callbacks,cases):
  need(row['label']==expected['label'] and row['requestBody']==expected['requestBody'] and row['signature']==expected['signature'] and row['status']==200 and row['contentType'].split(';')[0]=='application/json' and json.loads(row['body'])==expected['response'],'FEISHU_ENCRYPTED_REQUEST_RESPONSE')
  signature=hashlib.sha256(('1700000000'+'fixture-nonce'+'fixture-encrypt-key'+row['requestBody']).encode()).hexdigest();need(signature==row['signature'],'FEISHU_SENT_SIGNATURE')
 need([r['generation'] for r in raw['inbound']]==[0,1] and [r['text'] for r in raw['inbound']]==['CHANNEL_INPUT_0','CHANNEL_INPUT_1'],'FEISHU_TWO_EVENTS')
 for generation in [0,1]:
  incoming='CHANNEL_INPUT_'+str(generation);outgoing='CHANNEL_OUTPUT_'+str(generation)
  matches=[m for m in raw['models'] if m.get('stream') is True and [v.get('content') for v in m.get('messages',[]) if v.get('role')=='user'][-1:]==[incoming]];need(len(matches)==1,'FEISHU_REAL_MODEL')
  posts=[r for r in raw['http'] if r.get('method')=='POST' and r.get('path')=='/open-apis/im/v1/messages']
  need(any(r['body'].get('receive_id')=='room' and r['body'].get('msg_type')=='post' and json.loads(r['body'].get('content','{}'))=={'zh_cn':{'content':[[{'tag':'text','text':outgoing}]]}} for r in posts),'FEISHU_REAL_OUTBOUND_REPLY')
 need([s['enabled'] for s in raw['steps'] if s.get('event')=='public-reload-response']==[False,True],'FEISHU_RELOAD')
 return {'challenge':json.loads(callbacks[0]['body']),'eventResponses':[json.loads(x['body']) for x in callbacks[1:]],'inputs':['CHANNEL_INPUT_0','CHANNEL_INPUT_1'],'outputs':['CHANNEL_OUTPUT_0','CHANNEL_OUTPUT_1']}


def driver_wire(rows,records,origins,*,feishu=False):
 """Match every business HTTP row to the actual shared observer's byte pairs."""
 requests={};responses={}
 for record in records:
  if record.get('kind')=='fixture-http-inbound-request':
   url=urlsplit(record['url'])
   if url.path not in ['/feishu/webhook','/api/module-host/describe','/api/module-host/call','/api/module-host/approvals/status','/api/module-host/approvals/resume']:continue
   need(record['id'] not in requests,'HTTP_TRACE_DUPLICATE_REQUEST');need(url.scheme+'://'+url.netloc in origins,'HTTP_TRACE_LISTENER_ORIGIN');requests[record['id']]=record
  elif record.get('kind')=='fixture-http-inbound-response':
   need(record['id'] not in responses,'HTTP_TRACE_DUPLICATE_RESPONSE');responses[record['id']]=record
 matched=set()
 for row in rows:
  reqhex=row['requestBodyHex'];reshex=row['responseBodyHex'];pairs=[]
  for key,request_ in requests.items():
   if key in matched or key not in responses:continue
   response=responses[key]
   if urlsplit(request_['url']).path!=row.get('path','/feishu/webhook') or request_['method']!=row.get('method','POST'):continue
   for body in [request_['body'],response['body']]:need(body['truncated'] is False and len(bytes.fromhex(body['rawBytesHex']))==body['bytes'],'HTTP_TRACE_BODY_BOUND')
   if request_['body']['rawBytesHex']!=reqhex or response['body']['rawBytesHex']!=reshex or response['status']!=row['status']:continue
   headers=request_['headers'];label=row.get('label')
   if not feishu:
    need(headers.get('authorization')==('Bearer foreign' if label=='describe-unauthorized' else 'Bearer test-token'),'HTTP_ACTUAL_AUTHORIZATION')
    if '/approvals/' in row['path']:
     expected_auth=None if label=='resume-missing-auth' else 'Bearer foreign-only' if label=='status-foreign-actor' else 'Bearer approve-only'
     need(headers.get('x-staffdeck-approver-authorization')==expected_auth,'HTTP_ACTUAL_APPROVER_AUTH')
   else:need(headers.get('x-lark-request-timestamp')=='1700000000' and headers.get('x-lark-request-nonce')=='fixture-nonce' and headers.get('x-lark-signature')==row['signature'],'FEISHU_ACTUAL_SIGNATURE_HEADERS')
   pairs.append(key)
  need(bool(pairs),'HTTP_ROW_NOT_IN_ACTUAL_TRACE');matched.add(pairs[0])
 extra=set(requests)-matched
 if feishu:
  first=min((requests[k]['time'] for k in matched),default=0)
  need(all(k not in responses and requests[k]['time']<=first and requests[k]['body']['rawBytesHex']==rows[0]['requestBodyHex'] for k in extra),'FEISHU_UNMATCHED_HTTP')
 else:need(not extra,'HTTP_UNEXPECTED_EXTRA_BUSINESS_REQUEST')
 return len(matched)

def feishu_outbound_wire(raw,records):
 requests={r['id']:r for r in records if r.get('kind')=='mock-http-request'};responses={r['id']:r for r in records if r.get('kind')=='mock-http-response'}
 for generation in [0,1]:
  outgoing='CHANNEL_OUTPUT_'+str(generation);found=[]
  for key,request_ in requests.items():
   if request_['method']!='POST' or urlsplit(request_['url']).path!='/open-apis/im/v1/messages' or key not in responses:continue
   body=request_['body'];response=responses[key];need(body['truncated'] is False and response['body']['truncated'] is False,'FEISHU_OUTBOUND_TRACE_BOUND')
   value=json.loads(bytes.fromhex(body['rawBytesHex']));content=json.loads(value.get('content','{}'))
   if value.get('receive_id')=='room' and value.get('msg_type')=='post' and content=={'zh_cn':{'content':[[{'tag':'text','text':outgoing}]]}}:
    reply=json.loads(bytes.fromhex(response['body']['rawBytesHex']));need(response['status']==200 and response['finished'] is True and reply['code']==0,'FEISHU_OUTBOUND_ACTUAL_RESPONSE');found.append(key)
  need(len(found)==1,'FEISHU_OUTBOUND_NOT_IN_WIRE')


def provider_wire(models,formal,records,*,expected_public=1):
 """Validate actual received mock requests and written public-model response bytes."""
 posts=[];gets=[];public=[]
 class Wire:
  def __init__(self,data):self.stream=io.BytesIO(data)
  def makefile(self,*args,**kwargs):return self.stream
 for row in records:
  need(row['responseTruncated'] is False,'PROVIDER_WIRE_BOUND')
  response=HTTPResponse(Wire(bytes.fromhex(row['responseWireHex'])));response.begin();payload=response.read();need(response.status==200,'PROVIDER_ACTUAL_STATUS')
  if row['method']=='POST':
   body=json.loads(bytes.fromhex(row['requestBodyHex']));posts.append(body)
   if body.get('stream') is True and 'HOST_MODEL_42' in canonical(body.get('messages',[])):
    need(response.getheader('content-type','').split(';')[0]=='text/event-stream','PROVIDER_SSE_TYPE')
    lines=[line[6:] for line in payload.decode().splitlines() if line.startswith('data: ')];need(lines[-1:] == ['[DONE]'],'PROVIDER_SSE_DONE');events=[json.loads(x) for x in lines[:-1]]
    need(len(events)==2 and events[0]['choices'][0]['delta']['content']=='BUSINESS_DENY_DONE' and events[1]['choices'][0]['finish_reason']=='stop','PROVIDER_ACTUAL_MODEL_BYTES');public.append(body)
  elif row['method']=='GET':
   headers={k.lower():v for k,v in row['headers']};auth=headers.get('authorization','');gets.append({'path':row['path'],'bearerKind':'approver' if auth=='Bearer approve-only' else 'actor' if auth=='Bearer actor-only' else 'other'})
 need(Counter(canonical(x) for x in posts)==Counter(canonical(x) for x in models) and Counter(canonical(x) for x in gets)==Counter(canonical(x) for x in formal),'PROVIDER_REQUEST_ORIGINALS')
 need(len(public)==expected_public,'PROVIDER_PUBLIC_MODEL_COUNT')

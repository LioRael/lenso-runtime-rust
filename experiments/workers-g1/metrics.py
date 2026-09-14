"""Read one load run's remote metrics; never turns missing telemetry into success."""
import datetime
import json
import os
from pathlib import Path
import sys
import urllib.request

load = json.loads(Path(sys.argv[1]).read_text())
query = '''query GetWorkersAnalytics($accountTag: string, $datetimeStart: string,
 $datetimeEnd: string, $scriptName: string) {
 viewer { accounts(filter: {accountTag: $accountTag}) {
 workersInvocationsAdaptive(limit: 100, filter: {scriptName: $scriptName,
 datetime_geq: $datetimeStart, datetime_leq: $datetimeEnd}) {
 sum { requests errors subrequests } quantiles { cpuTimeP50 cpuTimeP99 }
 } } } }'''
# Query whole-second boundaries so subsecond client timestamps do not exclude
# invocations whose analytics timestamp has only second precision. Keep other
# suites outside this expanded window.
start = datetime.datetime.fromisoformat(load['started'].replace('Z', '+00:00')).replace(microsecond=0)
end = datetime.datetime.fromisoformat(load['ended'].replace('Z', '+00:00')).replace(microsecond=0) + datetime.timedelta(seconds=1)
variables = {
    'accountTag': os.environ['CLOUDFLARE_ACCOUNT_ID'],
    'scriptName': os.environ['WORKERS_G1_SCRIPT'],
    'datetimeStart': start.isoformat(),
    'datetimeEnd': end.isoformat(),
}
request = urllib.request.Request(
    'https://api.cloudflare.com/client/v4/graphql',
    data=json.dumps({'query': query, 'variables': variables}).encode(),
    headers={'Authorization': 'Bearer ' + os.environ['CLOUDFLARE_API_TOKEN'],
             'Content-Type': 'application/json'},
)
with urllib.request.urlopen(request, timeout=45) as response:
    result = json.load(response)
if result.get('errors'):
    raise RuntimeError(json.dumps(result['errors']))
accounts = result['data']['viewer']['accounts']
rows = accounts[0]['workersInvocationsAdaptive'] if accounts else []
observed = sum(row['sum']['requests'] for row in rows)
print(json.dumps({
    'run': load['run'], 'sampled_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'script': variables['scriptName'], 'started': load['started'], 'ended': load['ended'],
    'query_start': variables['datetimeStart'], 'query_end': variables['datetimeEnd'],
    'expected_requests': load['requests'], 'observed_requests': observed,
    'count_matches': observed == load['requests'], 'rows': rows,
    'note': 'API-native CPU units. Counts may reflect delayed ingestion or adaptive sampling. A matching count alone cannot exclude unrelated traffic. Never average row quantiles.',
}, indent=2))

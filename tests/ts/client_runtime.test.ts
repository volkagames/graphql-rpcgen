// The generated TypeScript client against a fetch that answers what each case
// says: what it sends for each kind of field and each form of operation, and
// how it tells a refusal from a raw answer. `ts_check` compiles this beside
// the `api.ts` generated from `field_kinds.graphql` and runs it under node.
//
// The expected texts are the ones `treat::rpc`'s Rust client writes for the
// same input — the server reads both with one table.

import {
  RpcClient,
  RpcError,
  createKindsOperations,
  type PathInput,
  type QueryInput,
  type RpcEventHandlers,
} from './api.js'

interface Sent {
  url: string
  method: string
  headers: Headers
  body: unknown
}

let sent: Sent[] = []
let answer: () => Response = () => envelope({ data: { ok: true } })

const fakeFetch: typeof globalThis.fetch = async (input, init) => {
  sent.push({
    url: String(input),
    method: init?.method ?? 'GET',
    headers: new Headers(init?.headers),
    body: init?.body,
  })
  return answer()
}

/** `EventSource` as far as the client uses it: the URL it was opened with. */
class FakeEventSource {
  static opened: string[] = []
  onmessage: ((message: MessageEvent) => void) | null = null
  onopen: (() => void) | null = null
  onerror: ((event: Event) => void) | null = null
  constructor(url: string) {
    FakeEventSource.opened.push(url)
  }
  close(): void {}
}
Object.defineProperty(globalThis, 'EventSource', { value: FakeEventSource, configurable: true, writable: true })

// Defaults naming a content type twice, in two cases: neither may reach a
// request whose body the client describes.
const client = new RpcClient({
  baseUrl: 'http://api.test/',
  fetch: fakeFetch,
  headers: { 'Content-Type': 'text/plain', 'content-type': 'text/html', 'X-Trace': 't1' },
})
const ops = createKindsOperations(client)

const handlers: RpcEventHandlers<{ line: string }> = { onEvent: () => {} }

function envelope(body: unknown, status = 200, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json', ...headers },
  })
}

function check(ok: boolean, what: string): void {
  if (!ok) throw new Error(what)
}

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual)
  const e = JSON.stringify(expected)
  check(a === e, `${what}:\n  got      ${a}\n  expected ${e}`)
}

/** The one request a case sent, and the log cleared for the next. */
function only(): Sent {
  equal(sent.length, 1, 'requests sent')
  const [request] = sent
  sent = []
  if (request === undefined) throw new Error('no request')
  return request
}

async function refused(run: () => Promise<unknown>, what: string): Promise<unknown> {
  try {
    await run()
  } catch (error) {
    return error
  }
  throw new Error(`${what}: expected a failure`)
}

async function typeError(run: () => Promise<unknown> | unknown, message: string): Promise<void> {
  const error = await refused(async () => run(), message)
  check(error instanceof TypeError, `${message}: not a TypeError: ${String(error)}`)
  equal((error as TypeError).message, message, 'TypeError message')
  equal(sent.length, 0, `${message}: nothing sent`)
}

async function rpcError(run: () => Promise<unknown>, status: number, code: string, what: string): Promise<void> {
  const error = await refused(run, what)
  check(error instanceof RpcError, `${what}: not an RpcError: ${String(error)}`)
  equal((error as RpcError).status, status, `${what}: status`)
  equal((error as RpcError).errors[0]?.code, code, `${what}: code`)
  sent = []
}

const cases: [string, () => Promise<void>][] = []
function test(name: string, run: () => Promise<void>): void {
  cases.push([name, run])
}

// Every kind once, as `treat`'s `client::query` spells it: a JSON-typed value
// is JSON text (the string `abc` as `"abc"`), only a list repeats its key, and
// absent values are left out.
const every = {
  id: 'a b',
  name: 'x&y',
  limit: 3,
  ratio: 1.5,
  flag: true,
  color: 'red',
  tags: ['p', 'q r'],
  counts: [1, 2],
  point: { x: 1, y: 2 },
  points: [
    { x: 1, y: 2 },
    { x: 3, y: 4 },
  ],
  data: 'abc',
  datas: ['a', null, 1],
} as QueryInput
const everyQuery =
  'id=a+b&name=x%26y&limit=3&ratio=1.5&flag=true&color=red&tags=p&tags=q+r&counts=1&counts=2' +
  '&point=%7B%22x%22%3A1%2C%22y%22%3A2%7D' +
  '&points=%7B%22x%22%3A1%2C%22y%22%3A2%7D&points=%7B%22x%22%3A3%2C%22y%22%3A4%7D' +
  '&data=%22abc%22&datas=%22a%22&datas=null&datas=1'

test('an RPC call is JSON under exactly application/json', async () => {
  const out = await ops.get({ id: 'u1', limit: 2 })
  equal(out, { ok: true }, 'answer')
  const request = only()
  equal(request.url, 'http://api.test/get', 'url')
  equal(request.method, 'POST', 'method')
  equal(request.headers.get('content-type'), 'application/json', 'content-type')
  equal(request.headers.get('x-trace'), 't1', 'the caller\'s other headers')
  equal(request.body, '{"id":"u1","limit":2}', 'body')
})

test('a query string spells each field as its table says', async () => {
  await ops.search(every)
  const request = only()
  equal(request.method, 'GET', 'method')
  equal(request.url, `http://api.test/search?${everyQuery}`, 'url')
})

test('a JSON field is quoted, a non-list array is one value, an unlisted field plain', async () => {
  const odd = {
    id: 'i',
    name: ['a', 'b'],
    data: 5,
    point: null,
    flag: undefined,
    extra: ['e'],
    other: 'x y',
  } as unknown as QueryInput
  await ops.search(odd)
  equal(
    only().url,
    'http://api.test/search?id=i&name=%5B%22a%22%2C%22b%22%5D&data=5&extra=%5B%22e%22%5D&other=x+y',
    'url',
  )
})

test('a JSON list spells null; any other list refuses it', async () => {
  await typeError(
    () => ops.search({ id: 'i', tags: ['x', null] } as unknown as QueryInput),
    'list field `tags` has a `null` item, which no query parameter spells',
  )
  await typeError(
    () => ops.form({ id: 'i', counts: [null] } as unknown as QueryInput),
    'list field `counts` has a `null` item, which no query parameter spells',
  )
  await typeError(
    () => ops.watch({ id: 'i', tags: [undefined] } as unknown as QueryInput, handlers),
    'list field `tags` has a `null` item, which no query parameter spells',
  )
  await ops.search({ id: 'i', datas: [null, 1] } as QueryInput)
  equal(only().url, 'http://api.test/search?id=i&datas=null&datas=1', 'url')
})

test('a form body is the query string under exactly the form type', async () => {
  await ops.form(every)
  const request = only()
  equal(request.method, 'POST', 'method')
  equal(request.url, 'http://api.test/form', 'url')
  equal(request.headers.get('content-type'), 'application/x-www-form-urlencoded', 'content-type')
  equal(request.body, everyQuery, 'body')
})

test('a path parameter is a segment, spelled as its kind', async () => {
  await ops.item({ id: 'a b/c', n: 7, tags: ['t'] })
  const request = only()
  equal(request.url, 'http://api.test/items/a%20b%2Fc/7?tags=t', 'url')
  equal(request.body, undefined, 'body')
  await typeError(
    () => ops.item({ id: '..', n: 1 }),
    'path parameter `id` cannot be `..`: no URL carries it as a segment',
  )
  await typeError(() => ops.item({ n: 1 } as PathInput), 'path parameter `id` is missing')
  await typeError(() => ops.item({ id: 'x', n: null } as unknown as PathInput), 'path parameter `n` is missing')
})

test('a JSON body beside a path parameter leaves it out, under application/json', async () => {
  await ops.put({ id: 'k', tags: ['a'], point: { x: 1, y: 2 } })
  const request = only()
  equal(request.method, 'PUT', 'method')
  equal(request.url, 'http://api.test/put/k', 'url')
  equal(request.headers.get('content-type'), 'application/json', 'content-type')
  equal(request.body, '{"tags":["a"],"point":{"x":1,"y":2}}', 'body')
})

test('a subscription spells its query string from its table', async () => {
  FakeEventSource.opened = []
  ops.watch(every, handlers).close()
  ops.watchPlain({ id: 'p q', limit: 4, flag: false }, handlers).close()
  equal(
    FakeEventSource.opened,
    [`http://api.test/watch?${everyQuery}`, 'http://api.test/watch_plain?id=p+q&limit=4&flag=false'],
    'urls',
  )
})

test('a raw request sends the declared type, or the one the caller picks', async () => {
  const body = new Blob(['1,2'])
  const out = await ops.upload({ id: 'u', tags: ['a', 'b'], meta: 'm', body })
  equal(out, { ok: true }, 'answer')
  const request = only()
  equal(request.url, 'http://api.test/upload?id=u&tags=a&tags=b&meta=%22m%22', 'url')
  equal(request.headers.get('content-type'), 'text/csv', 'content-type')
  check(request.body === body, 'the body is the blob itself')

  await ops.upload({ id: 'u', body }, { contentType: 'application/json' })
  equal(only().headers.get('content-type'), 'application/json', 'picked content-type')

  answer = () => envelope({ errors: [{ code: 'invalid_body' }] }, 415)
  await rpcError(() => ops.upload({ id: 'u', body }), 415, 'invalid_body', 'a refused upload')
})

test('a raw response request is JSON under exactly application/json', async () => {
  answer = () => new Response('a,b', { headers: { 'content-type': 'text/csv' } })
  await ops.download({ id: 'd' })
  const request = only()
  equal(request.url, 'http://api.test/download', 'url')
  equal(request.headers.get('content-type'), 'application/json', 'content-type')
  equal(request.body, '{"id":"d"}', 'body')
})

const refusal = { errors: [{ code: 'not_found', message: 'gone' }] }

test('bytes are the answer, unread', async () => {
  answer = () => new Response('a,b', { headers: { 'content-type': 'text/csv' } })
  const response = await ops.download({ id: 'd' })
  equal(await response.text(), 'a,b', 'body')
})

test('a JSON answer that is an envelope with errors is a refusal', async () => {
  answer = () => envelope(refusal)
  await rpcError(() => ops.download({ id: 'd' }), 200, 'not_found', 'application/json')
  answer = () =>
    new Response(JSON.stringify(refusal), { headers: { 'content-type': 'Application/JSON; charset=utf-8' } })
  await rpcError(() => ops.download({ id: 'd' }), 200, 'not_found', 'any case, any parameters')
})

test('a JSON answer that is not a refusal is returned with its body intact', async () => {
  for (const body of [{ rows: [1, 2] }, { errors: [] }, { errors: 'nope' }, { data: { ok: true } }, [1]]) {
    answer = () => envelope(body)
    const response = await ops.download({ id: 'd' })
    equal(await response.json(), body, 'body')
    sent = []
  }
})

test('only application/json itself is peeked', async () => {
  for (const type of ['application/jsonl', 'application/json-seq', 'application/problem+json', 'text/plain']) {
    answer = () => new Response(JSON.stringify(refusal), { headers: { 'content-type': type } })
    const response = await ops.download({ id: 'd' })
    equal(await response.json(), refusal, `${type}: body`)
    sent = []
  }
})

test('x-rpc-status decides when present', async () => {
  answer = () => envelope(refusal, 200, { 'x-rpc-status': 'ok' })
  const response = await ops.download({ id: 'd' })
  equal(await response.json(), refusal, 'ok: body')
  sent = []
  answer = () =>
    new Response(JSON.stringify(refusal), { headers: { 'content-type': 'text/csv', 'x-rpc-status': ' Error ' } })
  await rpcError(() => ops.download({ id: 'd' }), 200, 'not_found', 'error')
  answer = () => new Response('a,b', { headers: { 'content-type': 'text/csv', 'x-rpc-status': 'error' } })
  await rpcError(() => ops.download({ id: 'd' }), 200, 'internal_error', 'error without an envelope')
})

test('a non-2xx answer is a refusal whatever its type', async () => {
  answer = () => envelope(refusal, 404)
  await rpcError(() => ops.download({ id: 'd' }), 404, 'not_found', '404 envelope')
  answer = () => new Response('<html>bad gateway</html>', { status: 502, headers: { 'content-type': 'text/html' } })
  await rpcError(() => ops.download({ id: 'd' }), 502, 'internal_error', '502 page')
})

test('a JSON answer past the peek limit is the operation\'s own', async () => {
  const limit = RpcClient.RAW_PEEK_LIMIT
  // An envelope with errors, declared longer than the limit: never read.
  answer = () => envelope(refusal, 200, { 'content-length': String(limit + 1) })
  const declared = await ops.download({ id: 'd' })
  equal(await declared.json(), refusal, 'declared: body')
  sent = []

  // Undeclared, and streamed past the limit in pieces: read up to it, then
  // returned whole.
  const text = `{"errors":[{"code":"not_found","message":"${'x'.repeat(limit + 100)}"}]}`
  const bytes = new TextEncoder().encode(text)
  answer = () => {
    let offset = 0
    const stream = new ReadableStream<Uint8Array>({
      pull(controller) {
        if (offset >= bytes.byteLength) {
          controller.close()
          return
        }
        controller.enqueue(bytes.slice(offset, offset + 8192))
        offset += 8192
      },
    })
    return new Response(stream, { headers: { 'content-type': 'application/json' } })
  }
  const streamed = await ops.download({ id: 'd' })
  equal(await streamed.text(), text, 'streamed: body')
  sent = []

  // One that ends within the limit is still read.
  const short = `{"errors":[{"code":"not_found","message":"${'x'.repeat(limit - 100)}"}]}`
  answer = () => new Response(short, { headers: { 'content-type': 'application/json' } })
  await rpcError(() => ops.download({ id: 'd' }), 200, 'not_found', 'within the limit')
})

for (const [name, run] of cases) {
  sent = []
  answer = () => envelope({ data: { ok: true } })
  try {
    await run()
  } catch (error) {
    throw new Error(`${name}: ${error instanceof Error ? error.message : String(error)}`)
  }
  console.log(`ok - ${name}`)
}
console.log(`${cases.length} cases passed`)

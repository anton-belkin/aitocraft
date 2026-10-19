import json
import io
from pathlib import Path
import tempfile
import unittest
import uuid
import zipfile
from unittest.mock import patch

from gridcraft_links import LinkStore, token_from_url, origin_from_file, ensure_server, installation_id
from desktop_bridge import choose_route, panel_route, previous_route


class LinkTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.book = self.root/'Выручка и прибыль.xlsx'
        with zipfile.ZipFile(self.book, 'w') as z:
            z.writestr('customXml/gridcraftAgent.xml', '<agentLayer><![CDATA['+json.dumps({
                'context': {'codex_thread_id': str(uuid.uuid4()), 'codex_thread_title': 'Creator'}})+']]></agentLayer>')
        self.store = LinkStore(self.root/'state')
        self.thread = str(uuid.uuid4())
        self.token, self.ticket = self.store.issue(self.book, self.thread, 'Link creator', self.root)

    def test_ticket_is_local_exact_and_does_not_change_the_workbook(self):
        before = self.book.read_bytes()
        self.assertEqual(token_from_url('gridcraft://open/'+self.token), self.token)
        self.assertEqual(self.store.read(self.token)['path'], str(self.book))
        self.assertNotEqual(origin_from_file(self.book)['threadId'], self.thread)
        self.store.bind(self.token, self.ticket)
        self.assertEqual(self.book.read_bytes(), before)
        for url in ['gridcraft://open/../../else', 'https://open/'+self.token,
                    'gridcraft://open/'+self.token+'?thread=other', 'gridcraft://user@open/'+self.token]:
            with self.assertRaises(ValueError): token_from_url(url)
        with self.assertRaises(ValueError): self.store.read('f'*48)
        record = self.store.root/'tickets'/f'{self.token}.json'
        ticket = json.loads(record.read_text())
        ticket['expiresAt'] = 0
        record.write_text(json.dumps(ticket))
        with self.assertRaises(ValueError): self.store.read(self.token)

    def test_explicit_link_wins_over_origin_and_previous_chat(self):
        binding = dict(self.ticket, token=self.token)
        origin = {'threadId': 'creator', 'title': 'Creator'}
        route = choose_route(origin, {}, {'creator', self.thread}, binding)
        self.assertEqual((route['status'], route['threadId']), ('linked', self.thread))
        missing = choose_route(origin, route, {'creator'}, binding)
        self.assertEqual(missing['status'], 'linkMissing')
        self.assertEqual(missing['threadId'], '')
        self.assertEqual(panel_route(missing)['threadId'], self.thread)
        fallback = dict(missing, status='fallback', threadId='new')
        self.assertEqual(choose_route(origin, fallback, {'creator', 'new'}, binding)['threadId'], 'new')
        different = dict(binding, token='new-token')
        self.assertEqual(choose_route(origin, fallback, {'new'}, different)['status'], 'linkMissing')
        self.assertEqual(choose_route(origin, fallback, {'new', self.thread}, binding)['threadId'], self.thread)

    def test_binding_survives_save_as_reopen_and_a_new_link_replaces_it(self):
        self.store.bind(self.token, self.ticket, 'gridcraft:1:2')
        saved = {'id': 'gridcraft:1:2', 'path': str(self.root/'saved-as.xlsx')}
        self.assertEqual(self.store.for_document(saved)['token'], self.token)
        reopened = dict(saved, id='gridcraft:2:5')
        self.assertEqual(self.store.for_document(reopened)['threadId'], self.thread)
        other_token, other = self.store.issue(self.book, str(uuid.uuid4()), 'Other', self.root)
        self.store.bind(other_token, dict(other, path=saved['path']), reopened['id'])
        self.assertEqual(self.store.for_document(reopened)['token'], other_token)

    def test_worker_restart_does_not_steal_fallback_from_another_link(self):
        binding = dict(self.ticket, token=self.token)
        doc = {'id': 'gridcraft:2:3', 'path': str(self.book)}
        journal = {'one': {'originId': 'creator', 'bindingId': self.token, 'documentPath': str(self.book),
                           'threadId': 'new', 'title': 'New', 'at': 1}}
        route = previous_route(journal, doc, 'creator', {'new'}, binding)
        self.assertEqual((route['status'], route['threadId']), ('fallback', 'new'))
        self.assertEqual(previous_route(journal, doc, 'creator', {'new'}, dict(binding, token='other')), {})
        self.assertEqual(previous_route(journal, doc, 'creator', {'new'}), {})

    def test_link_service_from_another_installation_is_not_reused(self):
        for identity, succeeds in [('another-installation', False), (installation_id(), True)]:
            response = io.BytesIO(json.dumps({'service': 'gridcraft-links-v1', 'installationId': identity}).encode())
            with patch('urllib.request.urlopen', return_value=response), patch('gridcraft_links.subprocess.Popen') as launch:
                if succeeds:
                    ensure_server()
                else:
                    with self.assertRaises(RuntimeError):
                        ensure_server()
                launch.assert_not_called()


if __name__ == '__main__':
    unittest.main()

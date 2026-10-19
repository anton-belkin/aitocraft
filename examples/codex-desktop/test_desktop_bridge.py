import unittest
from desktop_bridge import choose_route, delivery_key, deliver, find_delivery, previous_route


class DesktopRoutingTests(unittest.TestCase):
    def test_only_loaded_origin_is_matched_and_fallback_never_overwrites_it(self):
        origin = {'threadId': 'origin', 'title': 'Original chat'}
        self.assertEqual(choose_route(origin, {}, {'origin'})['status'], 'matched')
        missing = choose_route(origin, {}, {'unrelated'})
        self.assertEqual((missing['status'], missing['threadId']), ('missing', ''))
        fresh = dict(missing, status='fresh', threadId='fresh', title='New chat')
        self.assertEqual(choose_route(origin, fresh, {'fresh'})['threadId'], 'fresh')
        self.assertEqual(origin, {'threadId': 'origin', 'title': 'Original chat'})
        self.assertEqual(choose_route(origin, fresh, {'origin', 'fresh'})['threadId'], 'origin')
        # Changing the origin cannot keep sending into an earlier fallback chat.
        self.assertEqual(choose_route({'threadId': 'other'}, fresh, {'fresh'})['threadId'], '')

    def test_question_ids_are_separate_between_files_but_stable_after_reopen(self):
        question = {'id': 'q1', 'text': 'Why?', 'askedAt': 42, 'topic': 't1'}
        first = {'id': 'gridcraft:1:1', 'path': '/tmp/first.xlsx'}
        reopened = {'id': 'gridcraft:2:3', 'path': '/tmp/first.xlsx'}
        other = {'id': 'gridcraft:1:2', 'path': '/tmp/second.xlsx'}
        self.assertEqual(delivery_key(first, question), delivery_key(reopened, question))
        self.assertNotEqual(delivery_key(first, question), delivery_key(other, question))
        self.assertNotEqual(delivery_key(first, question), delivery_key(first, dict(question, askedAt=43)))
        self.assertEqual(delivery_key(first, dict(question, topic={'id': 't1', 'status': 'open'})),
                         delivery_key(first, dict(question, topic={'id': 't1', 'status': 'revise'})))

    def test_active_desktop_turn_receives_steering_and_idle_chat_receives_a_turn(self):
        class Server:
            def __init__(self, status): self.calls, self.status = [], status
            def rpc(self, method, params):
                self.calls.append((method, params))
                return {'data': [{'id': 'turn', 'status': self.status}]} if method == 'thread/turns/list' else {'ok': True}
        for status, expected in [('inProgress', 'turn/steer'), ('completed', 'turn/start')]:
            server = Server(status)
            method, _ = deliver(server, 'origin', 'question', 'stable-message-id')
            self.assertEqual(method, expected)
            self.assertEqual(server.calls[-1][1]['threadId'], 'origin')
            self.assertFalse(any(name in ('thread/resume', 'thread/start') for name, _ in server.calls))

    def test_save_as_reopen_and_worker_restart_do_not_repost_or_lose_the_fallback_chat(self):
        question = {'id': 'q3', 'asked_at': 42, 'text': 'B3?'}
        unsaved = {'id': 'gridcraft:1:2', 'path': None}
        journal = {'key': {'question': 'q3', 'askedAt': 42, 'documentId': unsaved['id'],
            'documentPath': None, 'originId': 'closed', 'threadId': 'fresh', 'title': 'New chat', 'at': 1}}
        saved = dict(unsaved, path='/tmp/saved.xlsx')
        self.assertEqual(find_delivery(journal, saved, dict(question, text='[B3](cell:Sheet1!B3)?')), 'key')
        journal['key']['documentPath'] = saved['path']
        reopened = dict(saved, id='gridcraft:2:3')
        self.assertEqual(find_delivery(journal, reopened, question), 'key')
        self.assertEqual(previous_route(journal, reopened, 'closed', {'fresh'})['threadId'], 'fresh')
        self.assertEqual(previous_route(journal, reopened, 'other', {'fresh'}), {})


if __name__ == '__main__': unittest.main()

import unittest

from app.handler.feed import FeedHandler


class FeedHandlerTest(unittest.TestCase):
    def test_declares_operational_and_mcp_operations(self) -> None:
        self.assertTrue(callable(FeedHandler.health))
        self.assertTrue(callable(FeedHandler.metrics))
        self.assertTrue(callable(FeedHandler.mcp))

class A2ADeliveryTest(unittest.IsolatedAsyncioTestCase):
    async def test_native_task_reuses_durable_delivery_without_exposing_payment_metadata(self):
        import json
        from types import SimpleNamespace
        from a2a.server.tasks import InMemoryTaskStore
        from a2a.types import Task, TaskStatus
        from fastapi import Request
        store = InMemoryTaskStore()
        await store.save(Task(id="app-task", context_id="app-context", status=TaskStatus(state="submitted")))
        class Repository:
            def delivery(self, run_id, request_hash):
                self.run_id = run_id
                return SimpleNamespace(response=json.dumps({"jsonrpc":"2.0", "id":"run-1", "result":{
                    "structuredContent":{"news":[], "paid":True, "intentId":"inbound-1"}}}))
        repository = Repository()
        handler = FeedHandler(repository, False, store)
        body = json.dumps({"jsonrpc":"2.0", "id":"rpc-1", "method":"message/send", "params":{
            "message":{"kind":"message", "messageId":"message-1", "role":"user", "taskId":"app-task",
                       "contextId":"app-context", "parts":[{"kind":"text", "text":"news"}]}}}).encode()
        async def receive():
            return {"type":"http.request", "body":body, "more_body":False}
        request = Request({"type":"http", "method":"POST", "path":"/a2a", "headers":[(b"x-run-id", b"run-1")]}, receive)
        response = await handler.a2a(request)
        task = Task.model_validate(json.loads(response.body)["result"])
        self.assertEqual(task.id, "app-task")
        self.assertEqual(task.context_id, "app-context")
        self.assertEqual(task.artifacts[0].parts[0].root.data["intentId"], "inbound-1")
        self.assertEqual(repository.run_id, "run-1")


if __name__ == "__main__":
    unittest.main()

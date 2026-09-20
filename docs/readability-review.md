# Readability inspection and repairs

The review targets maintenance cost in the new implementation, preserving the existing input and transport contract. It does not reformat the pinned Orca research sources.

| Finding | Repair | Required regression evidence |
|---|---|---|
| Dispatcher combined field validation, authentication, session lifecycle, backend execution, and capture in one nested function. | Move parameter validation to `native/common/request_validation.cpp`; retain urgent routing in `handle`, serialized work in `execute_serialized`, and optional capture in `append_observation`. | Native contract tests, cancellation and desktop smoke after rebuilding. |
| Pending-stop cleanup was duplicated in request handling and idle tick. | One `finish_pending_stop` method owns provider cleanup and lease release; its caller-lock requirement is explicit. | Stop bypasses blocked input; a second daemon cannot acquire the lease until cleanup. |
| Locks and timestamps were named `serial_`, `state_`, `last_`, and `operationStart_`, hiding purpose and units. | Use `operationMutex_`, `stateMutex_`, `lastActivity_`, and `operationStartedMilliseconds_`. | Compilation and unchanged contract responses. |
| A connection handler mixed frame parsing, disconnect watching, serialization, and socket draining. | Extract `read_request`, `should_cancel_on_disconnect`, and `write_response`; preserve the Windows unread-pipe drain rule. | Real named-pipe and Unix-socket smoke, disconnect cancellation. |
| Input-lease acquisition mixed Windows identity discovery with ownership arbitration; raw resources could be copied. | Separate current-user mutex naming, add token/SID scope cleanup, prohibit copying, and use resource-specific names. | Cross-daemon ownership contract test and native builds. |
| Compound endpoint predicates obscured which drag endpoint was missing. | Name `hasStart`, `hasEnd`, and `usesElementIndex`; name numeric range/integer validation separately. | Existing invalid-input contract tests. |

Platform workers inspect their own backend and adapter modules alongside functional repairs. Completed checks and unresolved acceptance tests remain in `validation.md`; this document does not imply unexecuted desktop validation passed.

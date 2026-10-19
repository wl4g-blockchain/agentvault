package io.agentvault.sdk.model;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;

final class ApprovalTest {
    @Test void approvalLifecycleNamesMatchTheWireContract() {
        assertEquals("PENDING", Approval.ApprovalStatus.PENDING.name());
        assertEquals("APPROVE", Approval.ApprovalDecisionKind.APPROVE.name());
    }
}

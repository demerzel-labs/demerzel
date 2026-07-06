"""Budget preflight (injected fake Budgets client) + cost ceiling governor logic."""

from prototype.budget import (
    DEFAULT_COST_CEILING,
    CostCeiling,
    preflight,
    read_cost_ceiling,
)


class FakeBudgets:
    def __init__(self, actual, limit):
        self._actual = actual
        self._limit = limit
        self.calls = []

    def describe_budget(self, AccountId, BudgetName):  # noqa: N803 - boto3 arg names
        self.calls.append((AccountId, BudgetName))
        return {
            "Budget": {
                "BudgetLimit": {"Amount": str(self._limit)},
                "CalculatedSpend": {"ActualSpend": {"Amount": str(self._actual)}},
            }
        }


class BoomBudgets:
    def describe_budget(self, **kw):
        raise RuntimeError("access denied")


def test_under_threshold_is_ok():
    client = FakeBudgets(actual=50, limit=100)
    st = preflight({"PROTOTYPE_BUDGET_THRESHOLD": "95"}, client=client, account_id="1")
    assert st.status == "ok"
    assert st.pct == 50.0
    assert not st.should_refuse


def test_at_or_above_threshold_refuses():
    client = FakeBudgets(actual=96, limit=100)
    st = preflight({"PROTOTYPE_BUDGET_THRESHOLD": "95"}, client=client, account_id="1")
    assert st.status == "exceeded"
    assert st.should_refuse
    assert "refusing to run" in st.message


def test_check_failure_proceeds():
    st = preflight({}, client=BoomBudgets(), account_id="1")
    assert st.status == "check_failed"
    assert not st.should_refuse
    assert "budget check failed" in st.message


def test_custom_budget_name_passed_through():
    client = FakeBudgets(actual=10, limit=100)
    preflight({"PROTOTYPE_BUDGET_NAME": "custom-budget"}, client=client, account_id="acct9")
    assert client.calls == [("acct9", "custom-budget")]


def test_status_dict_serializable():
    client = FakeBudgets(actual=50, limit=100)
    st = preflight({}, client=client, account_id="1")
    d = st.to_dict()
    assert d["status"] == "ok"
    assert set(d) >= {"budget_name", "status", "actual", "limit", "pct"}


def test_read_cost_ceiling_default_and_override():
    assert read_cost_ceiling({}) == DEFAULT_COST_CEILING
    assert read_cost_ceiling({"PROTOTYPE_MAX_COST": "7.5"}) == 7.5
    assert read_cost_ceiling({"PROTOTYPE_MAX_COST": "garbage"}) == DEFAULT_COST_CEILING


def test_nonpositive_cost_ceiling_falls_back_to_default():
    # A zero/negative ceiling would trip on the first call -> empty review, exit 0.
    assert read_cost_ceiling({"PROTOTYPE_MAX_COST": "0"}) == DEFAULT_COST_CEILING
    assert read_cost_ceiling({"PROTOTYPE_MAX_COST": "-3"}) == DEFAULT_COST_CEILING


def test_nonpositive_threshold_does_not_refuse_every_run():
    # A non-positive threshold would otherwise refuse every run (exit 2).
    client = FakeBudgets(actual=10, limit=100)
    st = preflight({"PROTOTYPE_BUDGET_THRESHOLD": "0"}, client=client, account_id="1")
    assert st.status == "ok"
    assert not st.should_refuse


def test_cost_ceiling_governor():
    c = CostCeiling(ceiling=2.0, grace=1.5)
    assert not c.soft_tripped(1.99)
    assert c.soft_tripped(2.0)
    assert not c.hard_tripped(2.9)
    assert c.hard_tripped(3.0)  # 2.0 * 1.5

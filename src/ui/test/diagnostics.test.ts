import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  buildMpptChartData,
  orderedMppts,
} from "../src/lib/mppts.ts";
import {
  diagnosticsResponse,
  inverterDiagnostics,
  mppt,
} from "./diagnosticsFixtures.ts";

describe("dynamic diagnostic MPPT view data", () => {
  it("uses the empty state only for an empty collection", () => {
    const response = diagnosticsResponse([[]]);
    const current = orderedMppts(response.inverters[0].rows[0].mppts);

    assert.deepEqual(current, []);
    assert.deepEqual(buildMpptChartData(response.inverters[0].rows).trackers, []);
  });

  it("keeps one observed tracker when every integer reading is zero", () => {
    const current = orderedMppts([mppt(7, 0, 0, 0)]);

    assert.deepEqual(current, [
      {
        tracker_number: 7,
        dc_power_w: 0,
        dc_current_ma: 0,
        dc_voltage_mv: 0,
      },
    ]);
  });

  it("orders many trackers numerically including tracker 255", () => {
    const trackers = [mppt(255), mppt(2), mppt(1)];
    const inverter = inverterDiagnostics([trackers]);

    assert.deepEqual(
      orderedMppts(inverter.latestMeasurement?.mppts ?? []).map(
        (item) => item.tracker_number,
      ),
      [1, 2, 255],
    );
    assert.deepEqual(buildMpptChartData(inverter.rows).trackers, [1, 2, 255]);
  });

  it("preserves sparse tracker IDs in historical chart rows", () => {
    const inverter = inverterDiagnostics([
      [mppt(5), mppt(1)],
      [mppt(255), mppt(5)],
    ]);
    const chart = buildMpptChartData(inverter.rows);

    assert.deepEqual(chart.trackers, [1, 5, 255]);
    assert.deepEqual(chart.rows, [
      {
        label: inverter.rows[0].label,
        mppt_5_dc_power_w: 500,
        mppt_1_dc_power_w: 100,
      },
      {
        label: inverter.rows[1].label,
        mppt_255_dc_power_w: 25_500,
        mppt_5_dc_power_w: 500,
      },
    ]);
    assert.equal(chart.trackers.includes(2), false);
  });
});

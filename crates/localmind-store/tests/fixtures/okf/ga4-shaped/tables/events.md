---
type: BigQuery Table
title: Events table
description: Sharded daily and intraday Google Analytics 4 event export.
resource: https://console.cloud.google.com/bigquery?d=ga4_obfuscated_sample_ecommerce&t=events_
tags: [events, analytics, bigquery, ecommerce]
timestamp: 2026-05-28T22:53:05+00:00
---

# Events table

The `events_` table holds one row per event, sharded as `events_YYYYMMDD`.
See the [purchase join](../references/joins.md) for revenue attribution.

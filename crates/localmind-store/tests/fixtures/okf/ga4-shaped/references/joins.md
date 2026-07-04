---
type: Runbook
title: Purchase revenue join
tags: [join, revenue]
---

# Purchase revenue join

Join the [events table](../tables/events.md) on `event_name = 'purchase'` and
sum `ecommerce.purchase_revenue` grouped by day.

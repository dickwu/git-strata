| 来源 | 原意 | 目标模块 | 状态 | 理由 |
| --- | --- | --- | --- | --- |
| layerfs-content/tests/backing_custody.rs::a_failed_partial_append_keeps_its_physical_bytes_charged | run 文件部分追加失败后，已落盘字节仍计在账上（子进程内施加主机文件大小限制） | — | 不保留 | 实现细节（排序 run 的 FileBacking 记账）：主机文件大小限制下的部分追加；strata 没有排序 run 临时文件 |
| layerfs-content/tests/backing_custody.rs::a_failed_release_is_final_and_keeps_its_cause_and_bytes | 释放 run 文件失败是终局：保留原始宿主错误与字节数，既不重放，也不归还别的路径的字节 | — | 不保留 | 实现细节（排序 run 的 FileBacking 记账）：释放失败后的账目与错误保留；strata 没有排序 run 临时文件 |
| layerfs-content/tests/backing_custody.rs::a_released_run_takes_no_further_rows | 已释放的 run 不再接收追加，被拒的追加不占账 | — | 不保留 | 同上 |
| layerfs-content/tests/chunk_predecessor.rs::every_chunk_is_offered_the_base_extent_covering_its_own_range | 分块新版本对照前一版时，每个新 chunk 拿到基版中覆盖同一字节区间的 extent 作为 delta 提示 | — | 不保留 | 实现细节（delta 编码）：按字节位置给 chunk 配 predecessor 提示；strata 既不切块也不做 delta |
| layerfs-content/tests/chunk_predecessor.rs::a_base_less_build_is_unchanged | 无 base 的构造与显式传空 base 的构造逐字节相同 | — | 不保留 | 同上 |
| layerfs-content/tests/chunk_predecessor.rs::an_identical_version_is_offered_its_own_chunks | 内容相同的新版本，每个 chunk 的 predecessor 就是它自己的旧 chunk | — | 不保留 | 同上 |
| layerfs-content/tests/chunk_predecessor.rs::a_whole_file_base_offers_nothing | 整文件对象充当 base 时不提供任何 chunk predecessor | — | 不保留 | 同上 |
| layerfs-content/tests/chunk_predecessor.rs::the_correspondence_reads_mapping_pages_and_no_payload | 建立 chunk 对应关系只读两页映射页、不读 payload（读取代价断言） | — | 不保留 | 实现细节（delta 编码）：predecessor 对应关系的读取代价；strata 不切块也不做 delta |
| layerfs-content/tests/chunk_predecessor.rs::a_base_the_provider_cannot_serve_is_a_refusal | provider 取不到 base 时构造失败并返回 MissingObject | — | 不保留 | 同上 |
| layerfs-content/tests/chunk_predecessor.rs::a_branched_base_mapping_is_walked_and_a_grown_result_stops_at_its_end | base 的映射页是 branch（超过 128 个 extent）时逐层下降，结果比 base 长时末尾 chunk 不提供提示 | — | 不保留 | 同上 |
| layerfs-content/tests/compare_full_resume.rs::a_resumed_full_walk_lists_the_same_pairs_and_reads_from_its_cursor | 两棵树差异过大时 root_changes 全量游标续走：分页续走列出的差异对与一次走完相同，页内首对之前的读取量不随游标位置增长 | — | 不保留 | 实现细节（性能/资源预算）：compare 的游标续走与读取代价；strata 的 diff 是清单对清单的内存比较，没有游标 |
| layerfs-content/tests/compare_memo.rs::shared_pages_are_read_once_past_the_bound_and_evicted_pages_are_read_again | compare 的页 memo 超过 MEMO_PAGES 后保留各查找共享的上层页，被淘汰的页再读一次 | — | 不保留 | 实现细节（性能/资源预算）：compare 读取 memo 的淘汰策略；strata 的 diff 不经分页读取 |
| layerfs-content/tests/edit_backed.rs::backed_and_memory_routes_share_canonical_result_and_end_with_acknowledged_state | backed（索引记录）路径与内存路径产出同一 canonical 结果，并以已确认状态收尾 | — | 不保留 | 实现细节（known-edit 构造）：backed（索引记录）edit 路径的失败/确认顺序；strata 的文件是整文件 BLAKE3 blob，没有 edit 流 |
| layerfs-content/tests/edit_backed.rs::original_backing_discard_settle_and_post_accept_failure_end_without_another_job | backing 的丢弃/落定与接受后的失败都只保留原始错误，不再发起新的 job | — | 不保留 | 同上 |
| layerfs-content/tests/edit_backed.rs::malformed_mapping_record_and_reused_file_scope_never_fall_back_to_stored_objects | 畸形映射记录或被复用的 file scope 不会回退去读已存对象 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_backed.rs::original_consumer_refusal_leaves_attempt_marker_and_no_file_state_or_acknowledgement | 消费者拒收时留下一次尝试标记，不产出 FileState、也不确认 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_backed.rs::repeated_page_dag_reuses_real_stored_identity_without_conflating_private_drafts | 重复页 DAG 复用真实已存页的身份，同时不与私有草稿页混淆 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_backed.rs::three_repeated_replacement_pages_survive_both_boundary_joins_and_a_later_split | 三个重复替换页经两次边界合并与一次后续分裂后仍正确 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_backed.rs::no_op_and_whole_file_transition_keep_the_existing_dispatch_without_backing_effects | no-op 与整文件转换沿用既有分派，不触发 backing 副作用 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_backed.rs::resolved_repeated_child_cannot_hide_a_shifted_interior_mapping_boundary | 已解析的重复子页不能掩盖移位后的内部映射边界 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_backed.rs::file_state_refusal_and_post_accept_ack_refusal_retain_one_pending_attempt | FileState 被拒、接受后确认被拒，都只保留一次待定尝试 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_batch.rs::several_separated_edits_apply_in_order | 多处分离的 edit 按序应用，结果字节与根同全新构造一致 | — | 不保留 | 实现细节（known-edit 构造）：有序多 edit 流对字节向量模型的等价；strata 的文件是整文件 BLAKE3 blob，没有 edit 流 |
| layerfs-content/tests/edit_batch.rs::adjacent_edits_stay_separate_segments | 相邻 edit 保持为两个独立段 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_batch.rs::delete_only_streams_are_monotonic_in_result_coordinates | 纯删除流在结果坐标中单调 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_batch.rs::repeated_length_changes_accumulate | 反复改变长度的 edit 逐步累积 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_batch.rs::many_small_edits_are_applied_in_one_pass | 大量小 edit 一趟应用完 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_batch.rs::an_overlapping_stream_is_rejected_before_any_work | 重叠的 edit 流在做任何工作前被拒（InvalidEdit）；相邻、同起点的允许 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_batch.rs::every_object_the_edit_emits_is_reachable_from_its_root | edit 产出的每个对象都能从新根到达（没有孤儿对象） | — | 不保留 | 同上 |
| layerfs-content/tests/edit_bounds.rs::join_occupancy_at_every_boundary | 映射页在 63/64/127/128/129 个 extent 的边界合并后，每个非根页仍有 64..128 项、branch 根至少两个孩子 | — | 不保留 | 实现细节（映射页）：extent tree 的占用率、分页与层高；strata 无映射树 |
| layerfs-content/tests/edit_bounds.rs::streaming_flush_boundaries_and_height_growth | 流式构造在 192/193 个 extent 的 flush 边界与层高增长处仍规范 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_bounds.rs::a_join_of_two_full_pages_stays_canonical | 两个满页（80+100 extent）合并、删除接缝后映射划分仍规范 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_bounds.rs::many_files_and_many_edits_stay_bounded | 大量文件与大量 edit 下资源保持有界 | — | 不保留 | 实现细节（性能/资源预算）：edit 在大量 edit 下的保留前沿与延迟状态上界；strata 没有 edit 流 |
| layerfs-content/tests/edit_bounds.rs::the_retained_frontier_does_not_grow_with_the_edit_count | 保留前沿只取决于最终的树，不随 edit 次数增长（草稿被及时释放） | — | 不保留 | 同上 |
| layerfs-content/tests/edit_bounds.rs::a_source_that_ends_early_fails_without_emitting_a_root | 替换来源提前结束 → Io 错误，不产出根 | — | 不保留 | 实现细节（known-edit 构造）：替换来源与消费者失败的一次性返回；strata 的文件是整文件 BLAKE3 blob，没有 edit 流 |
| layerfs-content/tests/edit_bounds.rs::a_consumer_that_rejects_stops_the_edit_once | 消费者拒收（OutputRejected）→ edit 只停一次并原样返回该错误 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_bounds.rs::a_missing_base_root_is_reported_as_missing | base 根缺失 → MissingObject | — | 不保留 | 同上 |
| layerfs-content/tests/edit_bounds.rs::a_bounded_sink_that_fills_during_emission_fails_the_edit_once | 容量受限的 sink 在产出途中写满 → 只失败一次，已接受的前缀仍是子先于父 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_bounds.rs::a_thousand_separated_edits_keep_bounded_state | 1,024 处分离的 edit 保持有界状态且结果字节正确 | — | 不保留 | 实现细节（性能/资源预算）：edit 在大量 edit 下的保留前沿与延迟状态上界；strata 没有 edit 流 |
| layerfs-content/tests/edit_bounds.rs::the_frontier_does_not_grow_with_repeated_in_place_edits | 反复在同一小区域原地 edit 时，前沿仍与最终树成比例 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_bounds.rs::the_deferred_ceiling_charge_per_draft_stays_inside_its_derived_bound | 单个草稿的延迟状态记账不超过推导出的上界 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_cache.rs::more_than_two_cache_windows_evict_and_retained_pages_move_without_a_copy | 超过两个缓存窗口后淘汰旧页，保留的页以移动而非拷贝的方式交接 | — | 不保留 | 实现细节（映射页）：edit 读取映射页的导航缓存窗口；strata 无映射树 |
| layerfs-content/tests/edit_cache.rs::a_full_cache_survives_a_rejected_new_summary_without_retaining_the_page | 缓存已满时拒收错误的摘要，且不保留该页 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_cache.rs::cached_pages_still_check_each_requested_summary_without_another_demand | 缓存命中的页仍逐次核对所请求的摘要，不再发起读取 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_cache.rs::rehashed_malformed_or_short_nonroot_pages_are_never_retained | 重算哈希后畸形或过短的非根页永不进入缓存 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_cache.rs::provider_refusal_and_absence_end_the_original_demand_once | provider 拒绝或缺失时，原始读取只终止一次 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_indexed_view.rs::retained_view_and_fallible_runs_keep_memory_backed_and_transition_roots | 保留的 FileView 与可失败 runs 在内存/backed/表示转换路径上得到相同的根 | — | 不保留 | 实现细节（known-edit 构造）：已保留 FileView 上可失败的索引替换；strata 的文件是整文件 BLAKE3 blob，没有 edit 流 |
| layerfs-content/tests/edit_indexed_view.rs::original_indexed_length_refusals_stop_before_reads_backing_or_output | 索引长度被拒时，在读取、backing 与输出之前停止 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_indexed_view.rs::original_indexed_comparison_read_refusal_is_not_flattened_or_replayed | 比较读取被拒时保留原始错误，不拍平也不重放 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_indexed_view.rs::retained_view_no_ops_precede_occupied_backing_and_empty_edits_request_no_source | FileView 的 no-op 先于占用的 backing，空 edit 不请求来源 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_indexed_view.rs::borrowed_view_configuration_and_base_length_refuse_before_effects | 借用视图的配置或 base 长度不符时，在产生副作用前拒绝 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_indexed_view.rs::late_indexed_run_failure_retains_accepted_chunks_and_does_no_cleanup_job | 较晚的 run 失败时保留已接受的 chunk，且不做清理 job | — | 不保留 | 同上 |
| layerfs-content/tests/edit_indexed_view.rs::borrowed_view_consumer_refusal_keeps_original_object_and_hint_without_later_work | 消费者拒收时保留原始对象与提示，不做后续工作 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_lazy_plan.rs::assembly_reads_its_first_replacement_before_requesting_later_generated_rows | 装配先读第一条替换，再向生成器请求后续行 | — | 不保留 | 实现细节（known-edit 构造）：生成式 edit 输入的流式小结果装配；strata 的文件是整文件 BLAKE3 blob，没有 edit 流 |
| layerfs-content/tests/edit_lazy_plan.rs::first_assembly_failure_stops_before_later_rows_or_final_emission | 首条装配失败即停，不请求后续行，也不做最终产出 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_lazy_plan.rs::many_generated_deletions_retain_one_cursor_for_a_tiny_result | 大量生成的删除只保留一个游标，得到极小结果 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_localized.rs::an_extent_aligned_overwrite_reads_only_its_path_and_the_replaced_extents | 与 extent 对齐的覆盖写只读映射路径与被替换的 extent，未触及的叶页保持原身份 | — | 不保留 | 实现细节（known-edit 构造）：局部 edit 的读取/产出集合只含边界路径与被替换的 extent（按对象身份计数）；strata 的文件是整文件 BLAKE3 blob，没有 edit 流 |
| layerfs-content/tests/edit_localized.rs::a_mid_extent_overwrite_reads_only_the_straddling_extents | 落在 extent 中间的覆盖写只读被切开的两个 extent | — | 不保留 | 同上 |
| layerfs-content/tests/edit_localized.rs::localized_work_is_independent_of_file_size | 同样的 edit 在映射增长五倍的文件上，读取集合不随之增长 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_localized.rs::an_append_at_end_of_file_touches_only_the_right_boundary | 文件末尾追加只触及右边界 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_localized.rs::many_localized_edits_keep_each_edit_on_its_own_path | 连续六次 edit，每次各走各的路径，代价不随此前 edit 数增长 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_localized.rs::a_page_two_passes_reach_is_demanded_once_for_the_operation | 比较与构造两趟共用页 memo，一次操作内同一页只向 provider 要一次 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_localized.rs::a_pure_deletion_never_walks_the_rightmost_path | 纯删除不走最右路径（提示只由替换扫描消费） | — | 不保留 | 同上 |
| layerfs-content/tests/edit_localized.rs::an_overwrite_still_demands_the_predecessor_path | 覆盖写仍然需要 predecessor 路径（防止过度裁剪的反向钉子） | — | 不保留 | 同上 |
| layerfs-content/tests/edit_model.rs::a_small_file_matches_the_model_and_a_fresh_construction | 小文件 edit 的字节与全新构造的根同模型一致 | — | 不保留 | 实现细节（known-edit 构造）：edit 流对独立字节向量模型的等价；strata 的文件是整文件 BLAKE3 blob，没有 edit 流 |
| layerfs-content/tests/edit_model.rs::an_append_at_end_of_file_matches_the_model | 分块文件末尾追加必须到达尾部替换（回归用例） | — | 不保留 | 同上 |
| layerfs-content/tests/edit_model.rs::a_chunked_update_matches_the_model | 分块文件的更新与模型一致 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_model.rs::conversions_match_the_model_and_their_fresh_construction | 小→小、小→大、大→小的表示转换与模型及全新构造一致 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_model.rs::a_deletion_that_merges_two_slices_stays_canonical | 在单个 chunk 内部删除后，相邻的同 payload 切片必须合并为规范划分 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_model.rs::the_model_and_the_candidate_agree_on_degenerate_streams | 退化流（含相同替换）与模型一致；相同替换是 no-op | — | 不保留 | 实现细节（known-edit 构造）：退化 edit 流与模型的等价；其中'相同替换是 no-op'一条由 edit_noop 的改写行承担 |
| layerfs-content/tests/edit_noop.rs::an_empty_stream_returns_the_base_root_untouched | 没有任何 edit 时返回 base 根本身，只读取了 base 根一个对象 | overlay | 改写 | 改写为 overlay::compute：没有改动的工作区得到空 overlay，不产生新 blob（'只读一个对象'的读取代价断言不保留） |
| layerfs-content/tests/edit_noop.rs::an_equal_replacement_preserves_the_base_root | 用与原内容相同的字节替换一段 → 结果根与 base 相同，不重建结构 | overlay | 改写 | 改写为 overlay::compute：目录内容与基线逐字节相同 → 该路径没有 overlay 条目，也不新增 blob（对应'改回与基线相同即撤销覆盖'） |
| layerfs-content/tests/edit_noop.rs::several_equal_replacements_are_all_recognised | 多处相同替换全部被识别为 no-op | overlay | 改写 | 同上 |
| layerfs-content/tests/edit_noop.rs::a_shifted_equal_replacement_is_compared_in_current_coordinates | 插入使文件改变后，第二处相同替换要按当前坐标比较 | — | 不保留 | 实现细节（known-edit 构造）：edit 流的当前坐标系；strata 的文件是整文件 BLAKE3 blob，没有 edit 流 |
| layerfs-content/tests/edit_noop.rs::a_long_equal_prefix_then_a_mismatch_stops_the_comparison_early | 长段相同前缀后出现差异时，比较在差异处提前停止，不读完整个文件 | — | 不保留 | 实现细节（性能/资源预算）：compare-then-replay 的读取量上界；strata 的 overlay 用 size/gen count 预筛，只对判定已变的文件整文件哈希 |
| layerfs-content/tests/edit_noop.rs::a_length_changing_edit_is_never_treated_as_a_no_op | 改变长度的 edit 永远不按 no-op 处理 | — | 不保留 | 实现细节（known-edit 构造）：edit 的声明长度与区间长度之差；strata 的文件是整文件 BLAKE3 blob，没有 edit 流 |
| layerfs-content/tests/edit_noop.rs::an_out_of_range_replacement_length_is_an_error | 替换长度越界 → InvalidEdit | — | 不保留 | 同上 |
| layerfs-content/tests/edit_reference.rs::the_candidate_reproduces_the_reference_root_and_partition | 候选构造器对 9 个封存用例复现参考实现的根、页划分与存活叶子的身份 | — | 不保留 | 实现细节（映射页）：与封存 v0.1.6 参考实现的根/页划分对拍；strata 没有映射树，也没有可对拍的参考实现 |
| layerfs-content/tests/edit_reference.rs::the_oracle_fixtures_are_the_sealed_reference_revision | oracle 固件确实来自封存的参考修订版 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_reference.rs::an_interior_join_reads_its_boundary_child_once | 内部 join 对边界子页只读取并解码一次（nodes_read 计数钉子） | — | 不保留 | 实现细节（性能/资源预算）：映射页的读取次数计数；strata 没有映射树 |
| layerfs-content/tests/edit_runs.rs::mixed_runs_keep_the_byte_route_root_across_localized_and_representation_transitions | 混合 data/zero runs 在局部 edit 与表示转换下与字节路径得到同一根 | — | 不保留 | 实现细节（known-edit 构造）：含稀疏 zero run 的 edit 与零证据；strata 的文件是整文件 BLAKE3 blob，没有 edit 流 |
| layerfs-content/tests/edit_runs.rs::a_zero_replacement_above_four_gib_keeps_bounded_calls_and_exact_boundary_bytes | 超过 4 GiB 的零替换保持有界调用次数与精确的边界字节 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_runs.rs::a_large_zero_no_op_proves_real_repeated_pages_without_backing_or_byte_loops | 大段零替换经重复页证明为 no-op，不经 backing 也不逐字节循环 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_runs.rs::a_partial_zero_payload_never_certifies_its_nonzero_repeated_slice | 只部分为零的 payload 不会把其非零重复片段误证为零 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_runs.rs::zero_evidence_eviction_handles_more_than_sixty_four_distinct_payloads | 零证据缓存在超过 64 个不同 payload 时正确淘汰 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_runs.rs::zero_witness_reuse_rejects_changed_summary_and_underfilled_child_context | 复用零见证时，拒绝被改动的摘要与未填满的子页上下文 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_runs.rs::original_zero_evidence_refusal_and_absence_end_without_construction_or_fallback | 零证据读取被拒或缺失时只返回原始错误，不构造也不回退 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_runs.rs::malformed_runs_and_original_late_source_failure_preserve_one_attempt | 畸形 runs 与较晚的来源失败只保留一次尝试 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_runs.rs::sparse_emission_preserves_predecessor_and_refuses_occupied_scope_before_acceptance | 稀疏产出保留 predecessor，并在接受前拒绝已被占用的 scope | — | 不保留 | 同上 |
| layerfs-content/tests/edit_runs.rs::equal_length_comparison_end_is_one_invalid_replacement_without_output_or_backing | 等长比较提前结束 → 一次 InvalidEdit，无输出也无 backing | — | 不保留 | 同上 |
| layerfs-content/tests/edit_runs.rs::zero_payload_identity_role_and_shape_failures_end_on_the_original_demand | 零 payload 的身份/role/形状错误都终止于原始读取 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_scaling.rs::record_jobs_do_not_depend_on_the_length_and_grow_with_the_edits | backed edit 引擎的记录 job 数与文件长度无关、随 edit 数增长（只计数不计时） | — | 不保留 | 实现细节（性能/资源预算）：索引记录 job 的计数；strata 没有 backed edit 引擎 |
| layerfs-content/tests/edit_shift_scaling.rs::shift_edit_016_mib | 16 MiB 文件中部插入/删除 4 KiB：打印未触及子树的复用与 provider/consumer 工作量 | — | 不保留 | 实现细节（性能/资源预算）：在大文件中部插入/删除 4 KiB 时复用未触及子树的诊断计数（#316，非产品测试）；strata 不切块 |
| layerfs-content/tests/edit_shift_scaling.rs::shift_edit_064_mib | 同上（64 MiB） | — | 不保留 | 同上 |
| layerfs-content/tests/edit_shift_scaling.rs::shift_edit_256_mib | 同上（256 MiB，默认 #[ignore]） | — | 不保留 | 同上 |
| layerfs-content/tests/edit_shift_scaling.rs::shift_edit_512_mib | 同上（512 MiB，默认 #[ignore]） | — | 不保留 | 同上 |
| layerfs-content/tests/edit_single.rs::overwrite_in_the_middle | 文件中段覆盖写，与全新构造同终字节的根一致 | — | 不保留 | 实现细节（known-edit 构造）：对不可变 base 的单次 edit；strata 的文件是整文件 BLAKE3 blob，没有 edit 流 |
| layerfs-content/tests/edit_single.rs::overwrite_at_the_head_and_the_tail | 头部与尾部覆盖写 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_single.rs::insert_and_delete_change_the_length_exactly | 插入/删除精确改变长度 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_single.rs::append_at_end_of_file | 文件末尾追加 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_single.rs::complete_deletion_returns_the_empty_representation | 整段删除后得到空文件的规范表示（长度 0、字节为空、根等于全新构造的空文件） | store | 改写 | 改写为 store::put_blob/get_blob：零长度文件是合法 blob，往返得空字节，BlobId 恒等于空串的 BLAKE3 |
| layerfs-content/tests/edit_single.rs::the_base_objects_are_never_rewritten | edit 之后 base 的每个对象仍逐字节不变（不可变） | store | 改写 | 改写为 store::put_blob：写入新内容不改写已存 blob，已存 blob 的字节与 BlobId 不变（内容寻址 + append-only） |
| layerfs-content/tests/edit_single.rs::a_declared_length_that_does_not_match_its_bytes_fails | 声明长度与实际字节不符 → InvalidEdit | — | 不保留 | 实现细节（known-edit 构造）：对不可变 base 的单次 edit；strata 的文件是整文件 BLAKE3 blob，没有 edit 流 |
| layerfs-content/tests/edit_single.rs::an_inapplicable_range_is_rejected_before_any_work | 越界或重叠的区间在做任何工作之前被拒 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_single.rs::a_chunked_base_reads_and_edits_without_a_full_pass | 分块 base 的读取与 edit 不需要完整遍历 | — | 不保留 | 实现细节（known-edit 构造）：分块 base 上的局部 edit；strata 的文件是整文件 BLAKE3 blob，没有 edit 流 |
| layerfs-content/tests/edit_single.rs::an_edit_reads_and_decodes_the_base_root_once | edit 只读取并解码整文件 base 根一次（按 provider 的请求计数） | — | 不保留 | 实现细节（性能/资源预算）：base 根的读取次数计数；strata 没有 provider 读取 |
| layerfs-content/tests/edit_single.rs::an_edit_over_a_chunked_base_reads_and_decodes_the_base_root_once | 同上，分块 base（从视图已解码的状态重建映射树） | — | 不保留 | 同上 |
| layerfs-content/tests/edit_timing.rs::a_real_edit_reports_its_scopes_without_a_database | 真实 edit 在不开数据库的情况下上报 file.edit / edit.op 等计时 scope | — | 不保留 | 实现细节（性能/资源预算）：真实 edit 路径的计时 scope；strata 的验收数字另在各模块 §7 |
| layerfs-content/tests/edit_timing.rs::an_equal_replacement_reports_the_comparison_scope | 相同替换上报 comparison scope | — | 不保留 | 同上 |
| layerfs-content/tests/edit_timing.rs::recording_enabled_and_disabled_produce_the_same_result | 开启/关闭计时记录，工作量与结果完全相同 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_timing.rs::a_failing_edit_returns_its_original_error_and_an_error_outcome | 失败的 edit 原样返回其错误并记为 Error 结果 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_transitions.rs::exact_boundaries_at_every_accepted_cutoff | 在默认 128 KiB、256 KiB、1 MiB 三个 cutoff 的边界长度处，整文件/分块表示的转换点精确 | — | 不保留 | 实现细节（分块编码）：整文件与分块表示之间的 cutoff T 转换；strata 不切块 |
| layerfs-content/tests/edit_transitions.rs::every_conversion_direction_reaches_the_fresh_construction_root | 四个转换方向（小→小、小→大、大→小、任意→空）都得到与全新构造相同的根 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_transitions.rs::compressible_and_incompressible_inputs_agree_with_their_source | 可压缩与不可压缩的输入都与来源字节一致 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_transitions.rs::known_and_streamed_complete_lengths_agree | 已知长度与流式长度两个构造入口结果一致 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_transitions.rs::unsupported_cutoffs_are_rejected_and_the_accepted_ones_are_not | 不受支持的 cutoff → UnsupportedPolicy，受支持的通过 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_transitions.rs::whole_file_edit_emits_the_reference_bytes | 整文件 edit 产出的 canonical 字节与完整编码器逐字节相同 | — | 不保留 | 实现细节（对象信封）：整文件对象的 canonical 字节；strata 的 blob 是原始文件字节 |
| layerfs-content/tests/edit_transitions.rs::retained_segments_share_one_descent | 多个保留段共享一次映射根下降（根只读一次） | — | 不保留 | 实现细节（性能/资源预算）：保留段的映射页读取次数；strata 没有映射树 |
| layerfs-content/tests/edit_transitions.rs::straddling_payload_demanded_once_across_segments | 跨段的 payload 在整次装配中只读取一次 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_transitions.rs::the_transition_reads_only_what_it_keeps_and_charges_it | 表示转换只读取要保留的部分，并计入账 | — | 不保留 | 同上 |
| layerfs-content/tests/edit_transitions.rs::a_whole_file_result_reports_no_counters_and_that_is_recorded | 整文件结果不上报计数器，该缺口被显式记录 | — | 不保留 | 同上 |
| layerfs-content/tests/extent_cursor.rs::the_cursor_visits_every_extent_in_order_and_identity_equals_the_tree | extent 游标按序遍历文件的每个 extent；原始字节算出的 chunk 身份等于树里记录的 id；seek 每层只读一页 | — | 不保留 | 实现细节（映射页）：extent 游标与 chunk 身份；strata 没有 extent tree，也不切块 |
| layerfs-content/tests/extent_cursor.rs::a_whole_file_base_has_no_extents | 整文件对象的 base 没有 extent 视图 | — | 不保留 | 同上 |
| layerfs-content/tests/extent_cursor.rs::cut_candidates_include_the_boundaries_the_scanner_took | 切点候选扫描覆盖扫描器实际取的边界（约每 4096 个位置一个候选） | — | 不保留 | 实现细节（分块编码）：GEAR 切点候选扫描；strata 不切块 |
| layerfs-content/tests/extent_cursor.rs::an_edited_file_reports_slices_whose_bytes_do_not_hash_to_their_chunk | edit 后的文件会出现只含 chunk 一部分的切片，其字节哈希不等于所属 chunk 的身份，身份比较不会误认 | — | 不保留 | 实现细节（分块编码）：chunk 切片与身份比较；strata 不切块 |
| layerfs-content/tests/file_complete.rs::representation_boundaries_are_exact_and_exclusive | 按长度选择 Empty / WholeFile / Chunked 表示的 cutoff 边界精确且互斥 | — | 不保留 | 实现细节（分块编码）：cutoff T 决定的表示选择；strata 不切块 |
| layerfs-content/tests/file_complete.rs::empty_file_uses_the_defined_file_state_form | 空文件使用已定义的 file-state 形态，长度 0、extent 0，标识冻结 | — | 不保留 | 实现细节（映射页）：空文件的 file-state 形态；零长度文件作为 blob 的往返见 edit_single 的改写行 |
| layerfs-content/tests/file_complete.rs::one_byte_file_is_a_whole_file_object | 1 字节文件是单个 WholeFile 对象 | — | 不保留 | 实现细节（对象信封）：WholeFile 对象的 role；strata 的 blob 就是原始文件字节 |
| layerfs-content/tests/file_complete.rs::below_cutoff_uses_one_whole_file_object_and_reads_back_exactly | cutoff 以下的文件恰为一个 WholeFile 对象，并精确读回 | — | 不保留 | 实现细节（分块编码）：cutoff 以下的单对象表示；往返无损由 file_read 的改写行承担 |
| layerfs-content/tests/file_complete.rs::at_and_above_cutoff_uses_a_chunked_tree | cutoff 及以上走 FileState -> extent -> Chunk 树，不含 WholeFile | — | 不保留 | 实现细节（分块编码）：cutoff 及以上的分块树 |
| layerfs-content/tests/file_complete.rs::chunks_respect_the_frozen_size_window | chunk 长度落在 8192..32768 的冻结窗口内（尾块除外） | — | 不保留 | 实现细节（分块编码）：chunk 长度窗口 |
| layerfs-content/tests/file_complete.rs::chunk_partition_is_frozen_for_the_reference_input | 参考输入的 chunk 划分被冻结（6 块，长度 16396/17093/…） | — | 不保留 | 实现细节（分块编码）：冻结的 chunk 划分 |
| layerfs-content/tests/file_complete.rs::known_length_and_streaming_inputs_agree_exactly | 已知长度与流式两个构造入口，对同一字节得到相同的根、长度、对象字节与 role 序列 | store | 改写 | 改写为 store::BlobId：同一字节无论一次读入还是分块流式哈希，BlobId 相同（身份只取决于内容，与交付方式无关） |
| layerfs-content/tests/file_complete.rs::construction_is_deterministic_for_the_same_input | 同一输入两次构造，得到相同的根与相同的对象顺序 | store | 改写 | 改写为 store::put_blob 的幂等用例（blob_put_is_idempotent）：同一内容两次写入得到同一 BlobId，不新增 blob |
| layerfs-content/tests/file_complete.rs::compressible_and_incompressible_inputs_agree_with_their_source | 可压缩与不可压缩的输入都与来源字节一致 | — | 不保留 | 实现细节（分块编码）：不同数据形态下的 chunk 划分读回；往返无损由 file_read 的改写行承担 |
| layerfs-content/tests/file_complete.rs::streamed_source_requests_stay_bounded | 流式来源的请求大小与峰值对象字节保持有界 | — | 不保留 | 实现细节（性能/资源预算）：流式构造的内存上界；strata 的 put_blob 直接 clonefile，不读入内存 |
| layerfs-content/tests/file_complete.rs::late_input_failure_after_early_output_is_returned_once | 流式输入中途失败：只返回一次 Io 错误，先前已发出的 chunk 留在 consumer 里 | — | 不保留 | 实现细节（分块编码）：流式构造中途失败时已发出的 chunk 留存；strata 的 put_blob 要么整文件落盘、要么不落盘 |
| layerfs-content/tests/file_complete.rs::unsupported_policy_values_are_rejected_before_work | cutoff 或 depth 超出受支持范围 → UnsupportedPolicy，在做任何工作前拒绝 | — | 不保留 | 实现细节（分块编码）：cutoff/depth 策略校验；strata 没有这些参数 |
| layerfs-content/tests/file_complete.rs::whole_file_payload_is_classified_without_a_db | WholeFile 的 payload 不经数据库即可分类 | — | 不保留 | 实现细节（对象信封）：WholeFile payload 的 role 分类 |
| layerfs-content/tests/file_read.rs::full_read_returns_exact_bytes_for_every_representation | 对空/整文件/分块三种表示，整读都返回精确的原字节 | store | 改写 | 改写为 store::put_blob -> get_blob 往返：空、小、大文件读回的字节与原文件逐字节一致 |
| layerfs-content/tests/file_read.rs::ranges_cross_extents_and_page_boundaries | 范围读取跨越 extent 与映射页边界时取到正确字节 | — | 不保留 | 实现细节（范围读取）：按 extent/映射页取字节范围；strata 的 get_blob 整文件克隆，没有范围读取接口 |
| layerfs-content/tests/file_read.rs::empty_range_at_end_of_file_is_empty_and_at_eof_bound_is_allowed | 文件末尾的空范围返回空，EOF 边界合法 | — | 不保留 | 同上 |
| layerfs-content/tests/file_read.rs::out_of_bounds_and_inverted_ranges_fail | 越界或起止颠倒的范围 → InvalidRange | — | 不保留 | 同上 |
| layerfs-content/tests/file_read.rs::whole_file_root_rejects_a_range_beyond_its_payload | 整文件根拒绝超出其 payload 的范围 | — | 不保留 | 同上 |
| layerfs-content/tests/file_read.rs::repeated_demands_of_one_payload_read_it_once_and_emit_it_every_time | 重复内容使同一 chunk 在树中被引用多次：payload 只读一次，每次引用都输出 | — | 不保留 | 实现细节（分块编码）：chunk 级去重与重复引用；strata 只在整文件粒度去重 |
| layerfs-content/tests/file_read.rs::a_wave_is_released_before_the_next_one_is_read | 读取 wave 在读下一个 wave 前释放，同时存活的 payload 不超过 READ_WAVE_OBJECTS | — | 不保留 | 实现细节（acquisition profile）：读取 wave 的存活上界 |
| layerfs-content/tests/file_read.rs::a_provider_failure_is_returned_once_and_stops_the_read | provider 失败只返回一次 MissingObject 并终止读取，输出停在失败处 | store | 改写 | 改写为 store::get_blob：blob 文件缺失 -> 返回错误，目标路径不留下半个文件（clonefile 要么成功要么不创建） |
| layerfs-content/tests/file_read.rs::navigation_acquires_a_whole_level_per_provider_call | 导航按层成批取页：每个映射页只读一次，同层页在一次 provider 调用中到达 | — | 不保留 | 实现细节（acquisition profile）：按层分 wave 的映射页读取 |
| layerfs-content/tests/file_read.rs::a_large_read_acquires_payloads_in_bounded_batches | 24 MiB 分块文件读取按有界批次取 payload，最大批次落在声明窗口内 | — | 不保留 | 实现细节（acquisition profile）：payload 批次窗口 |
| layerfs-content/tests/file_read.rs::a_state_that_overstates_its_mapping_is_refused_not_partly_served | file state 与根页都多报 1 字节覆盖时，读取必须发现并拒绝，不得带着缺字节返回 Ok | — | 不保留 | 实现细节（映射页）：映射覆盖校验（InvalidRecord mapping coverage） |
| layerfs-content/tests/file_read.rs::a_state_that_contradicts_its_root_page_is_refused_before_the_walk | file state 与根页矛盾时，在遍历前就被拒绝 | — | 不保留 | 同上 |
| layerfs-content/tests/file_read.rs::a_leaf_slice_outside_the_chunk_grammar_is_refused_at_decode | 起点接近 u32 上限的叶页切片在解码处就被拒绝 | — | 不保留 | 实现细节（映射页）：叶页 extent 切片的语法校验 |
| layerfs-content/tests/file_read.rs::a_short_page_is_accepted_as_a_root_and_refused_as_a_non_root | 过短的页作为根可接受、作为非根被拒（NonCanonicalPagePartition） | — | 不保留 | 实现细节（映射页）：映射页的规范划分与根/非根语境 |
| layerfs-content/tests/file_read.rs::the_payload_wave_enforces_both_its_object_and_its_byte_ceiling | payload wave 同时执行对象个数与字节上限；超大 chunk 被拒 | — | 不保留 | 实现细节（acquisition profile）：payload wave 的对象/字节上限 |
| layerfs-content/tests/file_read.rs::a_range_read_serves_a_tree_with_more_than_one_branch_level | 范围读取能服务含多层 branch 的树（cumulative_logical_end 为页内累计） | — | 不保留 | 实现细节（映射页）：多层 branch 映射页的范围遍历 |
| layerfs-content/tests/filesystem_attributes.rs::reference_attribute_trees_are_reproduced_exactly | 属性树与封存的参考实现逐字节相同（根字节与对象集合） | — | 不保留 | 实现细节（文件系统树编码）：inode 属性树（domain+key 的 xattr 式数据）的页格式、补丁与值存放；strata 的 overlay 只记 mode，不带通用属性 |
| layerfs-content/tests/filesystem_attributes.rs::generic_domains_are_accepted_without_platform_dispatch | 任何结构合法的 domain 都被接受，值只当数据；空 domain、超长 domain/key、含 NUL 的 key 被拒 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_attributes.rs::opaque_values_survive_patches_without_being_decoded | 属性补丁只改被设置/删除的键，其余键保持原值根且不被解码 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_attributes.rs::portable_fields_round_trip_and_are_checked | portable 属性（mode、mtime）往返一致；超出文件类型掩码的 mode、超过十亿的小数秒被拒 | overlay | 改写 | 改写为 overlay::checkpoint/restore 的 mode 往返用例：OverlayEntry.mode（git 模式 0o100644/0o100755/0o120000）随条目记录并在 restore 时还原；mtime 不进 overlay，LayerFS 的 mode 掩码与小数秒校验不保留 |
| layerfs-content/tests/filesystem_attributes.rs::exact_sizing_partitions_match_the_encoder_and_the_tail_rebalances | 属性页按精确字节计划分，尾页再平衡；编码失败不当作页满信号 | — | 不保留 | 实现细节（文件系统树编码）：inode 属性树（domain+key 的 xattr 式数据）的页格式、补丁与值存放；strata 的 overlay 只记 mode，不带通用属性 |
| layerfs-content/tests/filesystem_attributes.rs::attribute_pages_reject_malformed_rows_and_reserved_fields | 属性页拒绝畸形行与保留字段 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_attributes.rs::values_are_extent_only_at_every_size | 属性值在各种大小下都以仅 extent 的形态存放，空值被拒 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_attributes.rs::a_missing_or_foreign_attribute_tree_fails_explicitly | 属性树缺失或来自别处 -> 明确失败 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_attributes.rs::a_reader_boundary_is_not_a_store_and_writes_are_rejected_once | 只读边界不是 Store，写入被拒一次（OutputRejected） | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_attributes.rs::an_inode_attribute_tree_is_read_through_a_path | 经路径读取 inode 的属性树：portable 字段、单个值、键列表，并计入读取工作量 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_attributes.rs::the_patch_route_and_a_from_scratch_build_agree_on_the_same_final_set | 补丁路径与从零构建在同一最终集合上得到同一个根 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_attributes.rs::every_attribute_descent_rejects_same_or_skipped_child_levels | 属性树下降时拒绝同层或跳层的子页 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_attributes.rs::the_metadata_forms_compare_by_value | inode 元数据的 root 形态与 inline 形态按值比较；mtime 或内容根不同才算改动 | — | 不保留 | 实现细节（文件系统树编码）：inode 元数据两种存放形态的按值比较；strata 的 overlay 只比较内容哈希与 mode，不比较 mtime（touch 不算改动，见 apfs 的 gen count 用例） |
| layerfs-content/tests/filesystem_bounds.rs::an_empty_tree_and_a_deep_tree_both_stay_inside_their_declared_ceilings | 空树无基线读取与排序工作；8 层深树也落在声明的资源上限内 | — | 不保留 | 实现细节（性能/资源预算）：整个操作的 owner 计数、变更路径的工作量与读取 wave；strata 不更新树 |
| layerfs-content/tests/filesystem_bounds.rs::a_small_change_reads_its_own_path_and_never_an_unrelated_subtree | 同样的三名字改名，在 500 项与 4000 项目录里工作量取决于路径与父扇出，而非树大小 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_bounds.rs::released_subtrees_are_traversed_in_bounded_pages_and_aliases_survive | 被释放的子树按有界页遍历；被移出的别名保留一个绑定 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_bounds.rs::every_reported_owner_is_nonzero_where_work_happened_and_zero_where_it_did_not | 每类资源 owner 的计数在有工作的地方非零、无工作的地方为零 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_bounds.rs::validation_reads_are_charged_to_the_operation | 校验读取计入操作的计数，而不是报告为零 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_bounds.rs::a_backing_too_small_for_the_declared_ceiling_is_refused_up_front | backing 容量低于声明的排序上限时，操作在 spill 之前就拒绝（ResourceUnavailable） | — | 不保留 | 实现细节（排序 run）：tiered ordering store 的外部排序行、spill 与 run 文件记账；strata 不做外部排序 |
| layerfs-content/tests/filesystem_bounds.rs::merge_inputs_and_output_are_covered_by_the_declared_ceiling | spill 合并时各层输入、输出与批次同时存在，都计入声明上限 | — | 不保留 | 实现细节（排序 run）：tiered ordering store 的外部排序行、spill 与 run 文件记账；strata 不做外部排序 |
| layerfs-content/tests/filesystem_bounds.rs::a_build_has_no_independent_cycle_walk_limit | 基线构造不再有独立的环检测游走上限，超过旧 4096 的构造也能成功 | — | 不保留 | 实现细节（文件系统树编码）：构造期的环检测游走上限；真实目录不会成环 |
| layerfs-content/tests/filesystem_bounds.rs::a_directory_larger_than_the_former_walk_ceiling_can_be_rebound | 条目数超过旧游走上限的目录仍可在同一父目录下改名，不触发游走 | — | 替代 | 替代：工作区是真实 APFS 目录，目录改名的代价与目录内条目数无关；overlay 对改名目录的呈现由 overlay 自己的 rename 用例承担 |
| layerfs-content/tests/filesystem_bounds.rs::the_release_frontier_reads_one_wave_per_batch_not_one_record_per_demand | 释放前沿按批次一个 wave 读取记录，而不是每个请求读一次 | — | 不保留 | 实现细节（acquisition profile）：释放前沿的记录读取批量化 |
| layerfs-content/tests/filesystem_bounds.rs::one_batched_demand_is_exactly_one_read_wave | 一次批量请求恰好计为一个 read wave | — | 不保留 | 实现细节（acquisition profile）：批量请求的 wave 计数语义 |
| layerfs-content/tests/filesystem_bounds.rs::a_joined_branch_page_reads_its_children_in_one_wave | 合并 branch 页时把未读子页作为一个有界组一次读取 | — | 不保留 | 实现细节（acquisition profile）：子页分组读取 |
| layerfs-content/tests/filesystem_bounds.rs::a_wide_branch_page_reads_the_children_a_change_reaches | 宽 branch 页只读变更触及的子页，没有 wave 宽于一个对象 | — | 不保留 | 实现细节（acquisition profile）：宽 branch 页的按需子页读取 |
| layerfs-content/tests/filesystem_bounds.rs::narrowing_keeps_a_small_scratch_working | scratch 预算缩小到 512 KiB 时 wave 收窄而不是拒绝，且结果根与读取的对象集合不变 | — | 不保留 | 实现细节（性能/资源预算）：scratch 租约下的 wave 收窄 |
| layerfs-content/tests/filesystem_bounds.rs::binding_lookups_are_batched_per_phase | 按阶段批量查找 binding：请求数随 k 增长而 wave 数只随表高度 | — | 不保留 | 实现细节（acquisition profile）：binding 查找的阶段批量化 |
| layerfs-content/tests/filesystem_bounds.rs::one_grouped_demand_is_one_charged_wave | 一次分组请求与一次点读各计一个 wave，互不重复计费 | — | 不保留 | 实现细节（acquisition profile）：wave 计费语义 |
| layerfs-content/tests/filesystem_bounds.rs::a_grouped_demand_is_one_wave_and_every_page_it_decoded | 分组请求计一个 wave，其解码的每个子页都计入 pages read | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_codec.rs::inode_pages_match_the_reference_bytes | inode 叶页与 branch 页的字节与封存的参考实现一致 | — | 不保留 | 实现细节（文件系统树编码）：页/根/符号链接的 golden-byte 编码与畸形输入拒绝；strata 的目录结构是真实目录加基线清单，不编码这些页 |
| layerfs-content/tests/filesystem_codec.rs::directory_pages_match_the_reference_bytes | 目录叶页与 branch 页的字节与参考实现一致 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_codec.rs::roots_and_symlinks_match_the_reference_bytes | 文件系统根与符号链接目标的字节与参考实现一致 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_codec.rs::attribute_pages_match_the_reference_bytes | 属性页的字节与参考实现一致 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_codec.rs::both_inode_leaf_routes_are_one_grammar | 池化的 InodeLeaf 路线与引擎自己的 inode 页路线是同一套语法，畸形输入得到同一个错误 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_codec.rs::malformed_and_reserved_input_is_rejected_once | 畸形、截断、含保留位或尾随字节的输入只被拒绝一次 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_codec.rs::roles_and_identity_are_not_interchangeable | 不同 role 的对象身份不可互换 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_codec.rs::a_directory_leaf_never_exceeds_the_page_ceiling | 目录叶页行数与字节数两个上界各自被执行，超出一律是划分拒绝 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_codec.rs::every_tree_role_checks_its_role_byte_and_its_flags | 每种树 role 都校验自己的 role 字节与 flag 字节，截断的对象在任何长度都解码失败 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_failure.rs::a_read_failure_at_each_boundary_publishes_no_root_and_keeps_the_old_one | 更新过程中任一读取边界失败：只报告一次失败，不发布新根，旧根仍可读，不遗留排序资源 | store | 改写 | 改写为 store::record_checkpoint 的原子性用例：写入中途失败不产生新 checkpoint，先前的 checkpoint 与 blob 仍可读（排序资源一项不保留） |
| layerfs-content/tests/filesystem_failure.rs::a_refusing_consumer_ends_the_operation_once_without_a_root | 逐个对象依次拒收：每次拒收都只终止操作一次，不接受任何根，不重试 | store | 改写 | 改写为 store 的写失败用例：第 k 次写入失败 -> 操作只终止一次、不留半个 checkpoint、不重放（设计不变量 6） |
| layerfs-content/tests/filesystem_failure.rs::a_malformed_or_unauthenticated_demanded_page_is_rejected_without_a_root | 被请求的页以错误身份提供，或根对象解码成异物/截断 -> 拒绝且不产生根，旧根仍可读 | store | 改写 | 改写为 store::get_blob 的完整性用例：读出内容的 BLAKE3 与 BlobId 不符 -> 读取失败，不把坏字节交给调用方（types 的 Code 没有专门的完整性码，归入 Unknown 还是新增由 store 文档决定） |
| layerfs-content/tests/filesystem_failure.rs::invalid_input_is_refused_before_anything_is_emitted | 无效输入（binding、serial、值）在发出任何对象之前被拒 | — | 不保留 | 实现细节（文件系统树编码）：目录/inode 更新输入的校验；strata 不更新树 |
| layerfs-content/tests/filesystem_failure.rs::cancellation_leaves_no_owned_ordering_resources_and_repeatable_state | 读取器中途失败后，不遗留排序字节与 run 文件，之后的真实更新仍可正常运行 | — | 不保留 | 实现细节（排序 run）：tiered ordering store 的外部排序行、spill 与 run 文件记账；strata 不做外部排序 |
| layerfs-content/tests/filesystem_failure.rs::a_binding_serial_outside_the_stored_range_is_refused | 超出存储范围（i64::MAX 以上）的 binding serial 被拒 | — | 不保留 | 实现细节（文件系统树编码）：inode serial 的存储范围 |
| layerfs-content/tests/filesystem_failure.rs::the_attribute_value_bound_is_the_chunk_maximum_at_its_boundary | 属性值上限恰为 chunk 最大值：边界内的值读回完整，超过即被拒 | — | 不保留 | 实现细节（文件系统树编码）：属性值的上限 |
| layerfs-content/tests/filesystem_failure.rs::a_branch_that_misstates_its_summary_is_refused_by_the_tree_engine | branch 页错报子树计数、字节总数或层级时，由更新引擎在读到子页后拒绝 | — | 不保留 | 实现细节（文件系统树编码）：branch 页的汇总校验 |
| layerfs-content/tests/filesystem_hardlinks.rs::two_aliases_reach_one_derived_count | 不同目录下的两个别名得到同一个派生计数，第三个别名再加一 | — | 替代 | 替代：工作区是真实 APFS 目录，硬链接计数由文件系统自己维护；overlay 按文件内容记录，不跟踪硬链接关系（git 也不跟踪） |
| layerfs-content/tests/filesystem_hardlinks.rs::a_move_across_directories_keeps_the_count_and_the_inode | 跨目录移动不改变 inode 与计数，旧路径不再存在 | — | 替代 | 替代：真实目录的 rename(2) 保持 inode 与链接数；overlay 对移动的呈现（Renamed{from,blob}，它同时是旧路径的墓碑）由 overlay 的 rename 用例承担 |
| layerfs-content/tests/filesystem_hardlinks.rs::a_child_moved_out_of_a_removed_subtree_survives | 把子文件移出后再删除其所在目录（同一操作），该文件存活，计数为 1 | — | 替代 | 替代：真实目录里 mv 后 rm -r，被移出的文件不受影响；由 APFS 与 shell 命令本身承担 |
| layerfs-content/tests/filesystem_hardlinks.rs::removing_one_alias_leaves_the_other_and_removing_the_last_removes_it | 删除一个别名保留另一个，删除最后一个才真正移除 | — | 替代 | 替代：真实目录的 unlink(2) 只减链接数，最后一个才释放；由 APFS 承担 |
| layerfs-content/tests/filesystem_hardlinks.rs::a_regular_file_keeps_at_least_one_binding | 新建的 regular file 不带任何 binding 属于断开的记录，被拒（InvalidRecord） | — | 不保留 | 实现细节（文件系统树编码）：inode 更新输入的 binding 校验 |
| layerfs-content/tests/filesystem_hardlinks.rs::a_declared_new_inode_cannot_lose_a_binding | 声明为新建的 inode 不可能减少 binding，矛盾输入被拒 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_limits.rs::a_component_name_is_accepted_at_255_bytes_and_refused_at_256 | 路径分量 255 字节可接受、256 字节被拒（超长 PathLimitExceeded，空名 InvalidPath） | — | 替代 | 替代：工作区是真实 APFS 目录，超长名字由文件系统在创建时拒绝（ENAMETOOLONG，macOS NAME_MAX 同为 255 字节）；types::RepoPath 不另设长度上限（types.md 只校验空段、.、..、绝对路径、NFC、UTF-8），ENAMETOOLONG 在 types §6.3 的 errno 表里没有条目，由 apfs/gitio 文档补充归类 |
| layerfs-content/tests/filesystem_limits.rs::a_path_is_accepted_at_4096_bytes_and_refused_at_4097 | 整条路径 4096 字节可接受、4097 字节被拒 | — | 替代 | 替代：工作区是真实 APFS 目录，超长路径由文件系统在创建时拒绝（ENAMETOOLONG；macOS PATH_MAX 为 1024，与 LayerFS 的 4096 数值不同）；types::RepoPath 不另设总长上限，归类同上一行 |
| layerfs-content/tests/filesystem_limits.rs::a_path_is_accepted_at_256_components_and_refused_at_257 | 路径 256 个分量可接受、257 个被拒 | — | 不保留 | 实现细节（文件系统树编码）：LayerFS profile 的分量个数上限 256；strata 的 RepoPath 只受总长与分量长度约束 |
| layerfs-content/tests/filesystem_limits.rs::a_symlink_target_is_accepted_at_4096_bytes_and_refused_at_4097 | 符号链接目标 4096 字节可接受、4097 字节被拒 | — | 不保留 | 实现细节（文件系统树编码）：SymlinkTarget 语法的 4096 字节限额；符号链接在 strata 里如何记录由 overlay 文档规定，不沿用该语法限额 |
| layerfs-content/tests/filesystem_limits.rs::an_attribute_domain_and_key_are_accepted_at_their_bounds_and_refused_over | 属性 domain 64 字节、key 255 字节可接受，超出被拒 | — | 不保留 | 实现细节（文件系统树编码）：属性 domain/key 的语法限额；strata 不带通用属性 |
| layerfs-content/tests/filesystem_limits.rs::the_limits_this_suite_cannot_bound_are_named_with_their_reason | 无法用廉价固件触达的限额（层级 31、页 8192 字节、run 层数 32）以断言固定其取值 | — | 不保留 | 实现细节（文件系统树编码）：LayerFS 格式常量 MAXIMUM_TREE_LEVEL/PAGE_BYTES/LEVELS 的取值钉子 |
| layerfs-content/tests/filesystem_limits.rs::a_read_wave_is_accepted_at_4096_demands_and_refused_at_4097 | 一次读取 wave 4096 个请求可接受、4097 个在询问 provider 之前被拒 | — | 不保留 | 实现细节（acquisition profile）：读取 wave 的请求个数上限 MAXIMUM_READ_DEMANDS |
| layerfs-content/tests/filesystem_ordering.rs::records_are_fixed_width_and_every_malformed_field_is_rejected | 88 字节定长记录的每个畸形字段都被拒 | — | 不保留 | 实现细节（排序 run）：tiered ordering store 的外部排序行、spill 与 run 文件记账；strata 不做外部排序 |
| layerfs-content/tests/filesystem_ordering.rs::the_pending_threshold_changes_only_where_the_rows_live | pending 阈值只改变行存放的位置，不改变结果 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_ordering.rs::tier_carries_keep_the_newest_row_and_accumulate_counts | tier 进位保留同一 serial 的最新行并累加计数，只在旧 tier 中的 key 存活 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_ordering.rs::growth_is_reserved_before_it_happens_and_cleanup_returns_the_bytes | 增长先预留再发生，清理归还字节 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_ordering.rs::a_removal_that_fails_is_visible_instead_of_being_hidden_in_drop | run 文件删除失败时由 owner 记录，而不是在 drop 里被吞掉 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_ordering.rs::the_operation_ceiling_is_enforced_before_the_rows_are_written | 操作的排序上限在写行之前就被执行 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_ordering.rs::append_read_flush_and_release_failures_fail_the_operation_without_a_root | 追加、读回、flush、释放任一失败都使操作失败且不产生根 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_ordering.rs::a_successful_operation_releases_its_ordering_resources_once | 成功的操作恰好释放一次排序资源 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_ordering.rs::a_spilled_lookup_agrees_with_a_full_scan_of_every_tier | spill 后的查找与全量扫描每个 tier 的结果一致 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_ordering.rs::run_lookup_answers_every_serial_in_every_order | run 查找按升序、降序、交错的任意顺序都给出同一答案 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_ordering.rs::a_failed_run_removal_keeps_the_file_and_its_bytes_in_the_account | run 文件删除失败后，账目仍如实反映磁盘上还在占用（owns_storage 为真） | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_ordering.rs::a_stale_run_file_does_not_block_a_later_backing | 旧 backing 遗留的 run 文件不会让同目录下之后的所有 backing 永久失败 | store | 改写 | 改写为 store::put_blob 的崩溃遗留用例：上次崩溃遗留的临时文件不使之后的写入永久失败，也不被新写入采用或截断（新写入取一个未被占用的临时名），与目录里的无关文件互不影响 |
| layerfs-content/tests/filesystem_ordering.rs::a_spill_keeps_the_scans_of_tiers_above_its_level | spill 到第 k 层时，高于 k 的 tier 保留各自的 run 与扫描位置 | — | 不保留 | 实现细节（排序 run）：tiered ordering store 的外部排序行、spill 与 run 文件记账；strata 不做外部排序 |
| layerfs-content/tests/filesystem_ordering.rs::a_fresh_build_counts_bindings_without_ordering_runs | 全新构建统计 binding，不需要排序 run | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_ordering.rs::a_fresh_build_charges_its_count_array_to_the_ordering_ceiling | 全新构建把计数数组计入排序上限 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_ordering.rs::the_parent_membership_charges_parents_not_bindings | 父目录成员集合按父目录个数而非 binding 个数计费；少一个父目录的上限就在产生根之前拒绝 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_ordering.rs::a_high_pending_ceiling_runs_spill_free_to_the_byte_bound | pending 上限很高时可零 spill 运行到字节上限（floor(ordering_bytes/(2*ROW_BYTES))） | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_ordering.rs::carried_state_removes_the_zero_count_re_pass | touched_serials 带出的状态使零计数推导不再需要第二遍查找 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_ordering_consolidate.rs::consolidation_never_writes_into_a_run_it_merges | 合并 run 时只写入自己创建的 run，不写进被合并的输入（封印所有已存在的 run 后合并仍成功且行流不变） | — | 不保留 | 实现细节（排序 run）：tiered ordering store 的外部排序行、spill 与 run 文件记账；strata 不做外部排序 |
| layerfs-content/tests/filesystem_ordering_scan.rs::lookups_allocate_nothing_after_the_tiers_are_built | 各层 tier 建好后，任意混合的续读/重启查找都不再做堆分配 | — | 不保留 | 实现细节（排序 run）：tiered ordering store 的外部排序行、spill 与 run 文件记账；strata 不做外部排序 |
| layerfs-content/tests/filesystem_ordering_scan.rs::a_restarted_scan_returns_the_same_rows_as_a_fresh_one | 从游标之后回头查找，答案与从头扫描一致；任何 tier 都没有的 serial 是'不存在'而非错误 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_ordering_scan.rs::a_sparse_newer_tier_skips_its_gap_and_finds_older_rows | 稀疏的较新 tier 跳过空隙并找到较旧 tier 的行 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_parent_lookup.rs::inherited_children_resolve_and_list_by_parent_identity | 继承来的子项可按父身份解析与列出 | — | 不保留 | 实现细节（文件系统树编码）：目录更新时按父目录成批取值与归并顺序；strata 不更新树 |
| layerfs-content/tests/filesystem_parent_lookup.rs::unchanged_parents_share_reads_and_reuse_their_authenticated_values | 未变化的父目录共享读取并复用已认证的值（一个叶子的固件六次对象请求即可） | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_parent_lookup.rs::partial_parent_batches_preserve_values_new_directories_and_removals | 部分父目录批次保持值、新目录与删除的正确；已分配但未绑定的目录绝不进入最终树 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_parent_lookup.rs::parent_values_remain_correct_when_the_reference_reducer_spills | 归并器 spill 后父目录的值仍正确 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_parent_lookup.rs::parent_value_ordering_quota_matrix_is_explicit_and_canonical | 父值与子 binding 交错时的配额矩阵（八个格子在保留基线中成功） | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_profile.rs::the_profile_text_names_the_bounds_it_is_hashed_with | 命名空间 profile 文本由其声明的各上限推导，上限变动则 profile 标识随之变动 | — | 不保留 | 实现细节（对象信封）：profile 描述文本的哈希身份 |
| layerfs-content/tests/filesystem_profile.rs::a_foreign_profile_is_refused_before_a_root_exists | 构造器收到外来 profile 标识 -> 在产生根之前拒绝，并指名该 profile | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_qualify.rs::a_built_multi_page_root_qualifies_within_bounded_windows | 多页根在有界窗口内通过整根校验：每个 inode 一行、每个非根目录一个队列项，不整体驻留 | — | 不保留 | 实现细节（性能/资源预算）：整根校验的窗口与记录流量；strata 没有整根校验 |
| layerfs-content/tests/filesystem_qualify.rs::a_proof_admits_only_its_own_root_and_context | 校验证明只接纳它自己的根与上下文 | — | 不保留 | 实现细节（文件系统树编码）：对外来根做整根拓扑校验（inode 表、绑定、引用计数的一致性）；strata 的目录结构来自真实目录，没有外来根需要校验 |
| layerfs-content/tests/filesystem_qualify.rs::a_root_of_another_context_is_refused_before_any_record | 另一上下文的根、或根本不是根的对象，在读任何记录之前就被拒 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_qualify.rs::a_binding_to_a_serial_outside_the_table_is_refused | 绑定指向 inode 表之外的 serial（悬空名字，含根之下）被拒 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_qualify.rs::a_directory_bound_twice_is_refused_in_one_parent_or_two | 同一目录在同一父目录或两个父目录下被绑定两次，被拒 | — | 替代 | 替代：工作区是真实 APFS 目录，目录不能被硬链接，因此只有一个父目录 |
| layerfs-content/tests/filesystem_qualify.rs::every_cycle_is_refused | 后代绑定祖先、目录绑定自身、任何指向根的名字、互相绑定的孤岛，都按环被拒 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_qualify.rs::unreachable_and_miscounted_inodes_are_refused | 不可达的 inode，以及声明计数与实际绑定数不符，被拒 | — | 不保留 | 实现细节（文件系统树编码）：对外来根做整根拓扑校验（inode 表、绑定、引用计数的一致性）；strata 的目录结构来自真实目录，没有外来根需要校验 |
| layerfs-content/tests/filesystem_qualify.rs::invalid_inode_rows_and_a_missing_root_inode_are_refused | 根必须是未绑定的目录、只有普通文件可多次绑定；根 inode 缺失被拒 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_qualify.rs::an_inode_leaf_overlapping_its_left_sibling_is_refused | 与左兄弟重叠的 inode 叶页被拒（NonCanonicalOrdering） | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_qualify.rs::incoming_work_cannot_supply_missing_topology_or_inflate_a_proof | 外部传入的计数不能补足缺失的拓扑，也不能夸大证明 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_qualify.rs::provider_and_record_failures_return_no_proof | provider 或记录失败时不返回证明，且原始错误原样浮现 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_read.rs::resolve_stat_and_list_agree_on_the_same_tree | resolve、stat、list 对同一棵树互相一致；未绑定的名字是逻辑缺失（PathNotFound），列目录支持按数量分页与续接 | — | 替代 | 替代：工作区与基线都是真实目录，路径解析、stat 与列目录直接由文件系统（ls/stat）完成，不需要查询层 |
| layerfs-content/tests/filesystem_read.rs::an_unbound_name_is_not_provider_absence | 不存在的路径（逻辑缺失，PathNotFound）与 provider 缺对象（MissingObject）是两种不同的答案，不得混为一个错误 | types | 改写 | 改写为 types::Code 的区分用例：路径不存在报 NoSuchPath（overlay/search/session 返回）；overlay 引用的 blob 在 Store 里找不到属 Store 内部不一致，不得混报为 NoSuchPath |
| layerfs-content/tests/filesystem_read.rs::a_serial_the_table_lacks_is_not_provider_absence | 目录绑定了 inode 表里没有的 serial 时，按逻辑缺失而非 MissingObject 回答 | — | 不保留 | 实现细节（文件系统树编码）：inode 表的 lookup miss 语义；strata 没有 inode 表（缺失两类的区分见上一行） |
| layerfs-content/tests/filesystem_read.rs::listing_is_bounded_by_bytes_as_well_as_count | 列目录同时受条数与字节数限制；容不下一行的字节上限是拒绝而不是'已列完' | — | 替代 | 替代：真实目录由 readdir 直接遍历；strata 的 JSON 输出是否分页由 cli 文档单独规定，不沿用该分页协议 |
| layerfs-content/tests/filesystem_read.rs::repeated_and_shared_demands_keep_order_and_cardinality | 成批 inode 请求共享 wave、保持顺序与重复项的基数；缺失的 serial 是 absent 而非 missing | — | 不保留 | 实现细节（acquisition profile）：inode 批量查找的 wave 共享 |
| layerfs-content/tests/filesystem_read.rs::a_symlink_reads_back_its_exact_stored_target | 符号链接读回其精确存储的目标；非符号链接被拒；畸形目标被拒 | overlay | 改写 | 改写为 overlay 的符号链接往返用例：符号链接以 mode 0o120000 记录（types::OverlayEntry），checkpoint -> restore 后链接目标逐字节不变（目标字符串的存放由 overlay 文档规定）；SymlinkTarget 的语法限额不保留 |
| layerfs-content/tests/filesystem_reference.rs::construction_matches_the_sealed_reference_roots_and_pages | 构造的根与完整可达对象集合（含页形状）与封存的参考结果逐一相同 | — | 不保留 | 实现细节（文件系统树编码）：与封存的参考实现对拍根与页形状；strata 没有可对拍的参考实现 |
| layerfs-content/tests/filesystem_reference.rs::updates_match_the_sealed_reference_operation | 更新操作的根与读回的逻辑状态与封存的参考结果一致 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_references_backed.rs::wide_multiwindow_direct_rows_remove_the_reducer_quota_and_match_canonical_roots | 宽的多窗口直接行去掉归约器配额，并得到与规范根相同的结果 | — | 不保留 | 实现细节（文件系统树编码）：backed 引用计数归约与子树释放（索引记录、FIFO 窗口、命名空间 frame、受保护写）；strata 不做引用计数归约 |
| layerfs-content/tests/filesystem_references_backed.rs::a_touched_row_pass_reads_a_wave_of_rows_in_jobs_of_twenty_one_and_matches_canonical_roots | touched-row 遍历按 21 行一个 job 成 wave 读取，结果与规范根一致 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_references_backed.rs::a_serial_moved_inside_one_window_is_read_once_and_written_once_from_its_first_result | 同一窗口内被移动的 serial 只读一次、只写一次，第二层含义应用在第一层的结果上 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_references_backed.rs::a_refused_window_writes_none_of_its_rows_and_keeps_the_whole_write_as_the_original | 窗口被拒时一行也不写，整笔写入保持为原始状态 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_references_backed.rs::a_batched_transfer_returns_its_first_items_refusal_and_writes_nothing | 批量转移返回第一项的拒绝并且不写任何东西 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_references_backed.rs::release_creates_lower_serial_rows_repeated_counts_and_surviving_aliases_without_losing_final_rows | 释放产生更低 serial 的行、重复计数与存活别名，且不丢失最终行 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_references_backed.rs::deep_release_persists_namespace_frames_and_finishes_all_fifo_windows | 深层释放持久化命名空间 frame，并完成全部 FIFO 窗口 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_references_backed.rs::complete_zero_seed_finishes_before_any_directory_release_even_across_windows | 所有独立的零种子在目录释放之前全部转移完成，即使跨窗口 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_references_backed.rs::required_row_touch_fresh_and_full_key_grammar_fail_without_final_output_or_later_calls | 必需行、touch、fresh 与完整 key 语法的失败不产生最终输出，也不再有后续调用 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_references_backed.rs::row_plus_touch_mutation_and_child_progress_are_atomic_and_keep_original_refusal_custody | 行 + touch 变更与子进度是原子的，并保留原始拒绝的监管 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_references_backed.rs::missing_work_window_is_not_false_completion_and_no_rows_are_dropped_for_final_scan | 缺失的 work 窗口不被当成完成，最终扫描时不丢行 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_references_backed.rs::rebuilt_released_directory_requires_same_save_capability_and_keeps_original_accepted_reader_error | 被重建的已释放目录需要同一次 Save 的能力，并保留原始已接受的读取错误 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_references_backed.rs::paged_release_keeps_actual_utf8_name_boundaries_and_all_repeated_parent_pages | 分页释放保持真实的 UTF-8 名字边界与所有重复的父页 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_references_backed.rs::missing_fresh_membership_is_refused_before_any_final_row_and_a_stale_row_guard_changes_nothing | 缺失的 fresh 成员在写任何最终行之前被拒，过期的行守卫不改变任何东西 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_references_backed.rs::new_child_inside_released_rebuilt_parent_remains_an_explicit_refusal_with_known_children_retained | 已释放又重建的父目录里的新子项保持为显式拒绝，已知子项被保留 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_references_backed.rs::authenticated_max_count_plus_one_is_length_overflow_before_release_or_final_root_on_both_routes | 认证过的计数上限加一是长度溢出，在释放与最终根之前就被拒 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_references_backed.rs::late_final_row_failure_is_forwarded_as_error_without_eof_finalization_or_later_demand | 较晚的最终行失败原样作为错误转发，不做 EOF 收尾，也不再有后续请求 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_rows.rs::a_spooled_update_publishes_the_same_root_as_resident_rows | 文件 spool 的更新与驻留行的更新发布同一个规范根 | — | 不保留 | 实现细节（RowSpool 文件溢写）：预备更新以文件行流代替驻留向量（固定 32 字节槽、payload、逐字节计费、完成后删文件）；strata 不更新树 |
| layerfs-content/tests/filesystem_rows.rs::a_wide_spooled_generation_applies_every_row_from_the_file | 宽的 spool 一代每一行都从文件应用 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_rows.rs::a_spool_that_does_not_hold_its_declared_rows_is_refused | spool 没有装下声明的行数时被拒 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_rows.rs::a_spool_refuses_a_row_beyond_its_declared_totals_or_capacity | spool 拒绝超出声明总数或容量的行 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_rows.rs::a_spool_refuses_a_key_that_does_not_rise_inside_its_run | spool 拒绝在 run 内 key 不上升的行 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_rows.rs::the_spool_file_is_removed_when_the_update_is_done | 更新完成后 spool 文件被删除 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_sorted.rs::initial_construction_needs_no_provisional_seed | 初始构造不需要临时的空种子，base 就是真实结果 | — | 不保留 | 实现细节（文件系统树编码）：sorted 引擎的边界：可选 base、填充规则、极小预算；strata 不更新树 |
| layerfs-content/tests/filesystem_sorted.rs::a_genuinely_empty_directory_is_emitted_once | 真正为空的目录只产出一次；没有变更的更新原样返回同一个根且不产出任何对象 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_sorted.rs::leaf_and_branch_boundaries_are_partitioned_canonically | 叶页与 branch 的边界按规范划分，根页除整棵树只有一页外必须填满 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_sorted.rs::an_unsorted_duplicate_or_repeating_change_is_refused_once | 未排序、重复或重复出现的变更只被拒一次 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_sorted.rs::a_tiny_supported_budget_works_or_refuses_before_allocating | 受支持的极小预算要么工作、要么在分配之前拒绝 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_sorted.rs::a_late_source_error_propagates_and_publishes_nothing | 来源较晚失败：错误原样传出，且不发布任何对象（sink 保持为空） | store | 改写 | 改写为 store::record_checkpoint 的失败用例：来源读取较晚失败 -> 错误原样传出，不落 checkpoint 行（已 put 的孤儿 blob 允许，留给 gc 回收） |
| layerfs-content/tests/filesystem_sorted.rs::page_widths_running_total_matches_the_recorded_partition | 页宽的运行总和与记录的划分一致（P1-12：O(k) 每次追加改为运行总和） | — | 不保留 | 实现细节（文件系统树编码）：sorted 引擎的边界：可选 base、填充规则、极小预算；strata 不更新树 |
| layerfs-content/tests/filesystem_sorted_localized.rs::inode_tables_are_the_trees_the_whole_page_merge_wrote | inode 表的树与整页合并写出的树一致（摘要冻结） | — | 不保留 | 实现细节（文件系统树编码）：sorted merge 只读变更触及的子页并写出与整页合并相同的树（按摘要冻结）；strata 不合并页 |
| layerfs-content/tests/filesystem_sorted_localized.rs::directories_are_the_trees_the_whole_page_merge_wrote | 目录的树与整页合并写出的树一致（摘要冻结） | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_sorted_localized.rs::a_three_leaf_change_in_an_eighty_leaf_page_demands_three_leaves | 80 叶页里改三个叶，只需读取这三个叶 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_sorted_localized.rs::a_leaf_that_underflows_demands_one_neighbour_and_no_other | 叶欠满时只读一个邻居，别无其他 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_sorted_localized.rs::a_recorded_total_its_children_cannot_hold_is_refused | 记录的总数与子页所能容纳的不符时被拒 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_state_backed.rs::backed_new_parent_membership_exceeds_only_the_old_map_allowance_and_drops_empty_parent | 新父成员超过旧 map 额度时 backed 路线仍可行，并丢弃空父目录 | — | 不保留 | 实现细节（文件系统树编码）：serial 状态的 backed 构造（新父成员、初始计数、流式最终行、受保护的状态记录）；strata 不更新树 |
| layerfs-content/tests/filesystem_state_backed.rs::backed_initial_counts_and_streamed_final_rows_preserve_real_alias_counts | backed 初始计数与流式最终行保持真实的别名计数 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_state_backed.rs::exact_context_marker_refuses_reuse_before_new_effects_and_retains_original_batch | 精确上下文标记在产生新效果之前拒绝复用，并保留原始批次 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_state_backed.rs::original_parent_count_and_post_accept_root_failures_stop_without_another_job | 原始的父计数失败与接受后的根失败，都不再发起别的 job 就停止 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_state_backed.rs::atomic_not_applied_and_malformed_root_are_refusals_not_fallbacks | '未应用'与畸形根都是拒绝，不是回退 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_state_backed.rs::missing_declared_count_or_directory_root_never_uses_a_default_or_supplied_draft_root | 缺失声明的计数或目录根时绝不使用默认值或调用方提供的草稿根 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_state_backed.rs::stale_serial_state_refuses_its_missing_guard_and_file_editor_kinds_remain_separate | 过期的 serial 状态拒绝其缺失的守卫，且文件编辑器的各 kind 保持分离 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_state_backed.rs::late_initial_count_read_failure_keeps_accepted_inode_pages_and_no_final_root | 较晚的初始计数读取失败时，保留已接受的 inode 页且不产生最终根 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_state_backed.rs::backed_update_consumes_rebuilt_roots_in_order_before_values_and_matches_move_alias_result | backed 更新先按序消费重建的根，再处理值，并与 move/alias 的结果一致 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_state_backed.rs::disappearing_replayed_root_cannot_be_overwritten_by_a_supplied_directory_value | 消失的重放根不能被调用方提供的目录值覆盖 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_streamed.rs::generated_wide_stream_matches_the_existing_resident_canonical_root | 生成的宽目录流得到与驻留规范根相同的根 | — | 不保留 | 实现细节（文件系统树编码）：目录流（生成式输入）共享规范构造：游标失败、计数/顺序/点位一致性、有效名字分页；strata 不更新树 |
| layerfs-content/tests/filesystem_streamed.rs::original_cursor_failure_stops_before_future_rows_objects_or_replay | 原始游标失败后，不再读取后续行、不产出对象、不重放 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_streamed.rs::counts_order_and_point_disagreement_refuse_before_final_output | 计数、顺序与点位互相矛盾时，在最终输出之前拒绝 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_streamed.rs::removed_and_unchanged_points_distinguish_a_move_from_a_second_parent | 移除点与未变点能区分'移动'与'第二个父目录' | — | 替代 | 替代：工作区是真实 APFS 目录，目录不能被硬链接，因此只有一个父目录 |
| layerfs-content/tests/filesystem_streamed.rs::paged_effective_names_match_the_resident_move_with_removals_and_insertions | 分页读取的有效名字与驻留路线的移动（含删除与插入）一致 | — | 不保留 | 实现细节（文件系统树编码）：目录流（生成式输入）共享规范构造：游标失败、计数/顺序/点位一致性、有效名字分页；strata 不更新树 |
| layerfs-content/tests/filesystem_streamed.rs::the_same_effective_cycle_validator_rejects_a_streamed_self_move | 同一个有效环校验器拒绝流式输入里的自我移动 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_streamed.rs::a_present_empty_update_retains_the_original_directory_and_root | 存在但为空的更新保留原目录与原根 | — | 不保留 | 实现细节（文件系统树编码）：目录流（生成式输入）共享规范构造：游标失败、计数/顺序/点位一致性、有效名字分页；strata 不更新树 |
| layerfs-content/tests/filesystem_timing.rs::a_recorded_update_reports_coarse_phases_and_changes_nothing | 记录计时的更新上报粗粒度 phase，且关闭计时时根与字节相同 | — | 不保留 | 实现细节（性能/资源预算）：文件系统树更新/构造的计时 phase；strata 的验收数字另在各模块 §7 |
| layerfs-content/tests/filesystem_timing.rs::a_recorded_construction_reports_its_phases | 记录计时的构造上报其 phase | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology.rs::the_root_stays_a_zero_count_directory | 根始终是零计数的目录：把根绑到别的目录之下被拒；根 inode 不是目录则无法构造 | — | 不保留 | 实现细节（文件系统树编码）：根 inode 的不变式 |
| layerfs-content/tests/filesystem_topology.rs::a_reused_identity_is_refused | 复用已存在的身份被拒 | — | 不保留 | 实现细节（文件系统树编码）：inode serial/身份复用校验 |
| layerfs-content/tests/filesystem_topology.rs::a_directory_or_symlink_cannot_have_two_parents | 目录或符号链接不能有两个父目录 | — | 替代 | 替代：工作区是真实 APFS 目录，目录不能被硬链接，只有一个父目录；符号链接的单绑定约束属 LayerFS 命名空间约束，overlay 不跟踪硬链接，该部分不保留 |
| layerfs-content/tests/filesystem_topology.rs::a_cycle_formed_by_moving_a_directory_below_itself_is_refused | 把目录移到它自己的子目录之下形成环，被拒 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology.rs::a_self_binding_is_refused | 目录绑定进自身被拒 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology.rs::a_disconnected_new_record_is_refused | 声明为新建却没有任何绑定的记录是断开的，被拒 | — | 不保留 | 实现细节（文件系统树编码）：新 inode 记录必须连通 |
| layerfs-content/tests/filesystem_topology.rs::a_caller_asserted_count_is_never_trusted | 调用方声明的引用计数从不被采信，计数由合并实际保留的绑定推导 | — | 不保留 | 实现细节（文件系统树编码）：引用计数由绑定推导 |
| layerfs-content/tests/filesystem_topology.rs::a_foreign_scope_or_profile_is_refused | 外来 scope 或 profile 的根被拒 | — | 不保留 | 实现细节（文件系统树编码）：根的 scope/profile 校验 |
| layerfs-content/tests/filesystem_topology.rs::a_base_resident_directory_cannot_gain_a_second_parent | base 里已有父目录的目录不能再获得第二个父目录 | — | 替代 | 替代：工作区是真实 APFS 目录，目录不能被硬链接，因此只有一个父目录 |
| layerfs-content/tests/filesystem_topology.rs::a_base_resident_directory_can_be_renamed_by_unbinding_first | 先解绑旧名字再绑新名字，base 里的目录可以合法改名，父目录仍只有一个 | — | 替代 | 替代：工作区是真实 APFS 目录，mv/rename(2) 对满足条件的移动本来就放行；overlay 对移动的呈现（Renamed{from,blob} 或 Deleted + Added）由 overlay 的 rename 用例承担 |
| layerfs-content/tests/filesystem_topology.rs::a_base_resident_symlink_cannot_gain_a_second_parent | base 里的符号链接不能再获得第二个父目录（派生计数为三） | — | 不保留 | 实现细节（文件系统树编码）：符号链接的单绑定约束；overlay 不跟踪硬链接（含符号链接的第二个名字） |
| layerfs-content/tests/filesystem_topology.rs::a_header_for_a_directory_the_base_dropped_is_a_missing_inode | 指向 base 已丢弃目录的 header 是缺失 inode，被拒 | — | 不保留 | 实现细节（文件系统树编码）：目录 header 与 inode 表的一致性 |
| layerfs-content/tests/filesystem_topology.rs::a_build_with_a_disconnected_directory_cycle_is_refused | 构建里两个目录只互相绑定（与根不连通）形成环，被拒 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology.rs::a_build_whose_root_is_not_declared_new_is_refused_up_front | 构建里根没有声明为新建，在动手前被拒 | — | 不保留 | 实现细节（文件系统树编码）：构建前置条件：根必须声明为新建 |
| layerfs-content/tests/filesystem_topology.rs::a_dropped_directory_emits_no_page_of_its_own | 同一最终状态批次里先绑定又丢弃的目录不产生页，也不进入归约 | overlay | 改写 | 改写为 overlay::compute 的最终状态用例：overlay 只看最终目录状态，先建后删的路径不留任何条目 |
| layerfs-content/tests/filesystem_topology.rs::a_cycle_formed_inside_a_build_is_refused | 无 base 的构建里 a 含 b、b 含 a，被拒 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology.rs::an_update_cycling_two_declared_new_directories_is_refused | 更新里两个新建目录互相包含形成环，被拒 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology.rs::an_update_with_a_disconnected_fresh_directory_cycle_is_refused | 更新里与根不连通的新建目录环被拒 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology.rs::a_cycle_through_a_fresh_directory_inside_the_moved_directory_is_refused | d -> p -> e -> d：新建的 p 在被移动目录内部，把 d 移到 p 之下形成环，被拒 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology.rs::a_permutation_of_65_directory_bindings_is_one_linear_walk | 65 个目录互换名字的置换是一次线性游走（步数为 placement 数的两倍而非平方） | — | 不保留 | 实现细节（性能/资源预算）：拓扑校验的游走步数、窗口与记录读写计数；strata 不更新树，没有这类校验 |
| layerfs-content/tests/filesystem_topology.rs::a_build_cycle_that_the_root_holds_is_refused | 根持有 a、a 与 b 互相包含的环，被拒 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology.rs::a_fresh_directory_bound_only_inside_a_dropped_directory_is_refused_by_an_update | 新目录只绑定在被丢弃的目录之内，更新被拒 | — | 不保留 | 实现细节（文件系统树编码）：最终状态描述的一致性；真实目录的最终状态天然一致 |
| layerfs-content/tests/filesystem_topology.rs::a_fresh_directory_bound_only_inside_a_dropped_directory_is_refused_by_a_build | 同样的描述在无 base 的构建里同样被拒，标签相同 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology.rs::a_stored_directory_bound_again_inside_a_dropped_directory_is_refused | 已存目录又被绑定到被丢弃的目录之内，被拒 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology.rs::a_stored_directory_moved_only_into_a_dropped_directory_is_refused | 已存目录只被移入被丢弃的目录之内（结果不可达），被拒 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology.rs::a_stored_symlink_bound_again_inside_a_dropped_directory_has_two_parents | 符号链接又绑定到被丢弃的目录之内，派生计数为二，被拒 | — | 不保留 | 实现细节（文件系统树编码）：符号链接的单绑定约束 |
| layerfs-content/tests/filesystem_topology_backed.rs::an_update_with_no_row_returns_the_base_root | 没有任何行的更新原样返回 base 根 | — | 不保留 | 实现细节（文件系统树编码）：backed 路线上的 no-op；该性质已由 filesystem_updates::a_same_name_no_op_keeps_the_root_identity 的改写行承担 |
| layerfs-content/tests/filesystem_topology_backed.rs::removing_a_name_the_base_does_not_bind_changes_nothing | 删除 base 里本来就没有绑定的名字：根不变，没有新增或移除的绑定 | overlay | 改写 | 改写为 overlay::compute 的墓碑用例：删除基线里不存在的路径（先建后删或从未存在）不产生墓碑，overlay 无变化 |
| layerfs-content/tests/filesystem_topology_backed.rs::restating_base_bindings_changes_nothing_and_walks_nothing | 重述 base 里已有的绑定不改变任何东西，也不游走 | — | 不保留 | 实现细节（文件系统树编码）：backed 路线上的 no-op；该性质已由 filesystem_updates::a_same_name_no_op_keeps_the_root_identity 的改写行承担 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_fresh_directory_with_an_empty_header_gets_a_real_empty_page | 带空 header 的新目录得到真实的空页，与 base 里的空目录页相同 | — | 不保留 | 实现细节（文件系统树编码）：空目录页的规范形式（strata 不记录空目录） |
| layerfs-content/tests/filesystem_topology_backed.rs::a_header_under_a_removed_base_directory_is_accepted_and_the_directory_released | 已被移除的 base 目录下的 header 被接受，该目录被释放 | — | 不保留 | 实现细节（文件系统树编码）：目录 header 语义 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_directory_value_keeps_its_supplied_root_without_a_header_and_is_rebuilt_with_one | 没有 header 的目录值保留其提供的根；有 header 时重建 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_backed.rs::supplied_link_counts_are_ignored | 调用方提供的链接计数被忽略 | — | 不保留 | 实现细节（文件系统树编码）：引用计数由绑定推导 |
| layerfs-content/tests/filesystem_topology_backed.rs::no_backed_refusal_depends_on_the_ordering_budget_and_state_stays_one_window | 任何 backed 拒绝都不依赖排序预算，状态始终只占一个窗口 | — | 不保留 | 实现细节（排序 run）：排序预算与窗口 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_wide_directory_change_is_classified_in_bounded_windows | 宽目录变更（150 删除、300 文件、50 目录）按有界窗口分类 | — | 不保留 | 实现细节（性能/资源预算）：拓扑校验的游走步数、窗口与记录读写计数；strata 不更新树，没有这类校验 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_deep_chain_of_fresh_directories_is_one_linear_proof_in_either_serial_order | 新建目录的深链在升序或降序 serial 下都是一次线性证明 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_move_deep_inside_a_stored_chain_lists_only_the_moved_subtree | 在已存链深处的移动只列出被移动的子树 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_backed.rs::sparse_changes_across_many_directories_match_the_resident_route | 分散在许多目录里的改名、新建、删除、移动与驻留路线一致，只列出被移动的目录 | — | 不保留 | 实现细节（文件系统树编码）：backed 与 resident 两条路线的等价 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_directory_placed_twice_in_one_batch_is_refused_before_anything_is_offered | 同一目录在一个批次里被放置两次，在任何东西被提供之前就被拒 | — | 替代 | 替代：工作区是真实 APFS 目录，目录不能被硬链接，因此只有一个父目录 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_stored_directory_bound_again_is_refused_by_its_derived_count_after_directory_pages | 已存目录被再次绑定，由其派生计数在目录页之后拒绝 | — | 替代 | 替代：工作区是真实 APFS 目录，目录不能被硬链接，因此只有一个父目录 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_symlink_bound_twice_is_refused_by_its_derived_count | 符号链接被绑定两次，由派生计数拒绝 | — | 不保留 | 实现细节（文件系统树编码）：符号链接的单绑定约束；overlay 不跟踪硬链接（含符号链接的第二个名字） |
| layerfs-content/tests/filesystem_topology_backed.rs::a_move_that_unbinds_first_is_accepted_and_a_restated_binding_is_no_placement | 先解绑再绑定的移动被接受，重述的绑定不算 placement | — | 替代 | 替代：工作区是真实 APFS 目录，mv/rename(2) 对满足条件的移动本来就放行；overlay 对移动的呈现（Renamed{from,blob} 或 Deleted + Added）由 overlay 的 rename 用例承担 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_directory_bound_inside_itself_is_a_cycle | 目录绑定进自己是环 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology_backed.rs::two_stored_directories_swapped_under_each_other_are_a_cycle | 两个已存目录各自移到对方之下是环 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_directory_moved_below_its_own_stored_child_is_a_cycle | 目录移到它自己的已存子目录之下是环 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_cycle_through_a_fresh_directory_inside_the_moved_directory_is_a_cycle | d -> p -> e -> d（p 为新建）是环（修正案 1 的反例） | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_cycle_of_fresh_directories_is_a_cycle | 若干新建目录互相包含是环 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_fresh_directory_bound_only_inside_a_dropped_directory_is_refused | 新目录只绑定在被丢弃的目录之内，被拒 | — | 不保留 | 实现细节（文件系统树编码）：最终状态描述的一致性；真实目录的最终状态天然一致 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_stored_directory_bound_or_moved_inside_a_dropped_directory_is_refused | 已存目录再绑定或只移入被丢弃的目录之内，被拒 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_stored_symlink_bound_again_inside_a_dropped_directory_has_two_parents | 符号链接又绑定到被丢弃的目录之内，有两个父目录，被拒 | — | 不保留 | 实现细节（文件系统树编码）：符号链接的单绑定约束 |
| layerfs-content/tests/filesystem_topology_backed.rs::inverting_a_parent_and_its_child_is_a_legal_move_without_a_base_walk | 把 /d/e 颠倒为 /e/d 是合法移动，且无需游走 base | — | 替代 | 替代：工作区是真实 APFS 目录，mv/rename(2) 对满足条件的移动本来就放行；overlay 对移动的呈现（Renamed{from,blob} 或 Deleted + Added）由 overlay 的 rename 用例承担 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_build_refuses_a_fresh_directory_bound_only_inside_a_dropped_directory_on_both_routes | 构建在两条路线上都拒绝只绑定在被丢弃目录内的新目录 | — | 不保留 | 实现细节（文件系统树编码）：最终状态描述的一致性；真实目录的最终状态天然一致 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_build_cycle_is_refused_and_a_deep_build_is_one_linear_proof | 构建里的环被拒；很深的构建只需一次线性证明 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环（'一次线性证明'的步数断言不保留） |
| layerfs-content/tests/filesystem_topology_backed.rs::a_deep_directory_renamed_in_place_under_a_stored_parent_walks_nothing | 已存父目录下的深层目录原位改名，只扫描一个父目录、不游走 | — | 不保留 | 实现细节（性能/资源预算）：拓扑校验的游走步数、窗口与记录读写计数；strata 不更新树，没有这类校验 |
| layerfs-content/tests/filesystem_topology_backed.rs::many_renames_in_one_parent_scan_that_parent_once | 同一父目录里的大量改名只扫描该父目录一次，记录点读随改名数线性增长而非平方 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_rename_in_place_beside_a_real_move_walks_only_the_moved_subtree | 原位改名与真实移动并存时，只列出被移动的子树 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_cycle_through_a_directory_renamed_in_place_is_still_a_cycle | 穿过原位改名目录的环仍然是环 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_rename_in_place_with_a_second_binding_elsewhere_has_two_parents | 原位改名的目录若在别处还有第二个绑定，则有两个父目录，被拒 | — | 替代 | 替代：工作区是真实 APFS 目录，目录不能被硬链接，因此只有一个父目录 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_rename_in_the_root_beside_a_placement_under_a_stored_directory_walks_nothing | 根内改名与已存目录下的新建并存时不游走 | — | 不保留 | 实现细节（性能/资源预算）：拓扑校验的游走步数、窗口与记录读写计数；strata 不更新树，没有这类校验 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_name_swap_of_two_stored_directories_in_one_parent_is_in_place | 同一父目录内两个已存目录互换名字属于原位操作 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_rename_with_the_old_name_recreated_walks_nothing | 改名后以旧名重建新目录，不游走 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_rotation_of_two_stored_directories_in_one_parent_walks_nothing | 同一父目录内两个目录轮换名字，不游走 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_cycle_through_a_directory_renamed_over_a_recreated_name_is_still_a_cycle | 穿过'改名后重建同名'目录的环仍然是环 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_rename_whose_directory_is_also_bound_elsewhere_still_has_two_parents | 改名的目录在别处还有绑定，则仍有两个父目录，被拒 | — | 替代 | 替代：工作区是真实 APFS 目录，目录不能被硬链接，因此只有一个父目录 |
| layerfs-content/tests/filesystem_topology_backed.rs::consecutive_windows_sharing_stored_children_read_them_once | 连续窗口共享已存子项时，每个子项只读一次 | — | 不保留 | 实现细节（acquisition profile）：窗口间共享已读记录 |
| layerfs-content/tests/filesystem_topology_backed.rs::two_moved_directories_each_under_the_others_stored_child_are_a_cycle | 两个被移动的目录各在对方的已存子目录之下是环 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology_backed.rs::one_moved_directory_under_another_ones_stored_child_is_accepted | 一个被移动的目录移到另一个目录的已存子目录之下是合法的，除非对方也被移走而构成环 | — | 替代 | 替代：工作区是真实 APFS 目录，mv/rename(2) 对满足条件的移动本来就放行；overlay 对移动的呈现（Renamed{from,blob} 或 Deleted + Added）由 overlay 的 rename 用例承担 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_ring_of_seventy_fresh_and_stored_directories_is_a_cycle | 70 个新建与已存目录构成的环被拒；去掉一个已存目录后成链，同样的 70 次 placement 被接受 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_ring_of_seventy_moves_through_territory_is_a_cycle | 70 次借道已存子目录的移动构成环，被拒 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 拒绝把目录移入自身或其子孙（EINVAL），目录树不可能成环 |
| layerfs-content/tests/filesystem_topology_backed.rs::a_legal_walk_as_long_as_the_step_bound_allows_is_accepted_and_linear | 合法的长游走在步数上界内被接受且是线性的 | — | 不保留 | 实现细节（性能/资源预算）：拓扑校验的游走步数、窗口与记录读写计数；strata 不更新树，没有这类校验 |
| layerfs-content/tests/filesystem_topology_backed.rs::the_refusal_labels_pinned_on_the_resident_route_hold_on_the_backed_route | 在 resident 路线上钉住的拒绝标签在 backed 路线上同样成立 | — | 不保留 | 实现细节（文件系统树编码）：backed 与 resident 两条路线的拒绝标签等价 |
| layerfs-content/tests/filesystem_topology_counts.rs::the_bases_grow_and_the_work_subtree_does_not | base 在未被命名的 bulk 子树里增长时，同一个小变更的校验工作量不增长 | — | 不保留 | 实现细节（性能/资源预算）：拓扑校验的游走步数、窗口与记录读写计数；strata 不更新树，没有这类校验 |
| layerfs-content/tests/filesystem_topology_counts.rs::one_file_value_update_costs_no_validation_work | 改一个文件的值不产生校验工作 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_counts.rs::one_hard_link_is_two_demands_and_no_placement | 新增一个硬链接 = 2 次请求、0 次 placement | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_counts.rs::one_new_file_is_two_demands_and_no_placement | 新建一个文件 = 2 次请求、0 次 placement | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_counts.rs::one_file_rename_is_two_demands_and_no_placement | 文件改名 = 2 次请求、0 次 placement | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_counts.rs::one_symlink_rename_is_two_demands_and_no_placement | 符号链接改名 = 2 次请求、0 次 placement | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_counts.rs::one_file_removal_is_one_demand | 删除一个文件 = 1 次请求 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_counts.rs::one_directory_removal_releases_its_subtree_without_validation_walks | 删除一个目录释放其整棵子树，且没有校验游走 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_counts.rs::one_directory_rename_within_the_root_is_one_placement_and_no_walk | 根内的目录改名 = 1 次 placement、无游走 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_counts.rs::one_directory_moved_under_a_fresh_root_directory_is_two_placements_and_no_walk | 目录移到根下的新目录里 = 2 次 placement、无游走 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_counts.rs::one_directory_moved_under_a_stored_directory_lists_only_its_own_subtree | 目录移入已存目录时只列出它自己的子树 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_counts.rs::a_directory_moved_under_a_fresh_directory_of_a_stored_one_lists_only_its_own_subtree | 目录移入'已存目录下的新目录'时只列出它自己的子树 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_counts.rs::a_change_wider_than_one_window_keeps_one_window_resident_at_every_size | 超过一个窗口的变更，在任何 base 大小下都只驻留一个窗口 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_counts.rs::one_directory_renamed_in_place_under_a_stored_directory_walks_nothing | 已存目录下的目录原位改名不游走 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_counts.rs::one_directory_renamed_with_its_old_name_recreated_walks_nothing | 目录改名后以旧名重建新目录，不游走 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_counts.rs::larger_bases_bound_the_sorted_merge_inode_reads_by_the_branch_fan_out | 更大的 base 下，sorted merge 的 inode 读取被 branch 扇出乘以高度所限定 | — | 不保留 | 同上 |
| layerfs-content/tests/filesystem_topology_model.rs::random_directory_batches_agree_with_an_independent_parent_model | 随机目录批次（在各处新建、移入根/已存目录/新目录/自身后代、原位改名、删除、互换、同名替换）与独立的父模型一致：每个目录最终恰有一个绑定或被移除，成环批次被拒 | — | 替代 | 替代：工作区是真实 APFS 目录，rename(2) 与目录单父约束天然保证这个模型的结论（无环、每个目录至多一个父） |
| layerfs-content/tests/filesystem_updates.rs::a_tiny_tree_is_created_and_read_back_exactly | 创建一棵极小的树并精确读回：根为目录、文件为 RegularFile、计数与内容根正确 | — | 替代 | 替代：工作区是真实目录，创建文件与目录后由文件系统自己读回；overlay 把新文件记为 Added（已进 git 索引）或 Untracked（未进索引且未被 ignore）的用例另见 overlay |
| layerfs-content/tests/filesystem_updates.rs::a_content_only_change_leaves_every_directory_page_untouched | 只改文件内容根时，没有任何绑定变化，所有目录页保持原身份 | — | 不保留 | 实现细节（文件系统树编码）：内容根变化不触及目录页（Merkle 页复用）；strata 不按页复用 |
| layerfs-content/tests/filesystem_updates.rs::a_same_name_no_op_keeps_the_root_identity | 用相同的最终绑定与相同的值再次更新：什么都不变，根身份不变 | overlay | 改写 | 改写为 overlay::compute 的幂等用例：工作区没有变化时重算得到完全相同的 overlay，不产生新差异 |
| layerfs-content/tests/filesystem_updates.rs::a_type_replacement_replaces_the_binding_and_the_record | 同名处把普通文件换成符号链接：绑定与记录都被替换，解析得到新的类型 | overlay | 改写 | 改写为 overlay::compute 的类型替换用例：同一路径由普通文件换成符号链接，overlay 记为 Modified（mode 由 0o100644 变为 0o120000），旧记录不残留 |
| layerfs-content/tests/filesystem_updates.rs::removing_the_last_binding_removes_the_inode_and_keeps_the_root_readable | 删除最后一个绑定：该名字不再存在，inode 从新表里消失，根仍可读 | overlay | 改写 | 改写为 overlay 的墓碑用例（deleted_base_file_is_tombstone）：删除基线文件 -> 该路径记墓碑，其余条目与根目录不受影响 |
| layerfs-content/tests/filesystem_updates.rs::an_unsorted_or_duplicate_change_list_is_refused_before_any_write | 未排序或重复的变更列表在写入任何东西之前被拒 | — | 不保留 | 实现细节（文件系统树编码）：目录/inode 更新输入的排序与唯一性语法 |
| layerfs-content/tests/fixture_seal.rs::the_sealed_fixtures_are_exactly_the_frozen_ones | 封存目录的哈希与冻结清单一致，增删改任何固件都会失败 | — | 不保留 | 实现细节（封存固件校验）：LayerFS 仓库对自己封存固件与生成器记录的完整性守卫；git-strata 没有封存固件 |
| layerfs-content/tests/fixture_seal.rs::the_generator_record_is_the_pinned_one | 生成器记录与代码里的常量一致，钉住的 blob 与恢复提交不会悄悄漂移 | — | 不保留 | 同上 |
| layerfs-content/tests/fixture_seal.rs::no_source_in_core_can_reseal_during_an_ordinary_test_run | core 下没有任何源文件能在普通测试运行中重新封存固件 | — | 不保留 | 同上 |
| layerfs-content/tests/fixture_seal.rs::an_altered_generator_record_is_reported | 被改动的生成器记录会被报告 | — | 不保留 | 同上 |
| layerfs-content/tests/fixture_seal.rs::a_writer_without_the_ignore_or_the_gate_is_reported | 缺少 #[ignore] 或环境闸门的写入者会被报告 | — | 不保留 | 同上 |
| layerfs-content/tests/fixture_seal.rs::a_gated_ignored_writer_and_plain_readers_are_not_reported | 带闸门且被忽略的写入者与普通读取者不会被误报 | — | 不保留 | 同上 |
| layerfs-content/tests/glob_directories.rs::recursive_directory_glob_includes_direct_children | `test/**/*.test.js` 匹配 test 下直接子文件与任意深度子目录里的 .test.js，不匹配其他目录、非 .test.js 或带前缀的名字 | overlay | 保留 | 保留：`.lww` 的 `**` 目录通配语义整体借用（LayerFS 的 Rule::glob 方言），向量（路径去掉前导 /）原样可用；落在 overlay::lww_matches |
| layerfs-content/tests/glob_directories.rs::recursive_directory_glob_requires_complete_directory_names | `src/**/index.js` 要求完整目录名（/src/web/index.js 可、/src/webindex.js 不可）；`**/index.js` 匹配根下 index.js；`src/*.js` 不跨目录 | overlay | 保留 | 保留：同上，落在 overlay::lww_matches |
| layerfs-content/tests/glob_directories.rs::last_writer_rules_share_recursive_directory_semantics | 从 /.lww 规则文件加载的规则共享同一套 `**` 语义（dist/**/bundle.js 匹配 dist/bundle.js 与 dist/web/bundle.js，不匹配 dist/webbundle.js） | overlay | 改写 | 改写为 overlay 的 .lww 加载用例：规则文件位置改为 .strata/lww，匹配语义不变 |
| layerfs-content/tests/glob_directories.rs::consecutive_recursive_directory_globs_include_zero_directories | 连续的 `**/**` 同样包含零个目录（src/**/**/index.js 匹配 /src/index.js），并经 .lww 规则文件加载得到同样的结果 | overlay | 改写 | 改写为 overlay 的 .lww 加载用例：同上；其中纯 glob 向量可原样沿用 |
| layerfs-content/tests/inode_decode_cost.rs::typed_inode_decode_does_not_allocate_an_intermediate_raw_row_vector | 带类型的 inode 页解码不分配中间的原始行向量（外部分配观测） | — | 不保留 | 实现细节（性能/资源预算）：inode 页解码的分配次数；strata 不解码 inode 页 |
| layerfs-content/tests/inode_leaf.rs::a_leaf_round_trips_with_its_exact_layout | 叶页往返并保持精确布局 | — | 不保留 | 实现细节（文件系统树编码）：紧凑 inode 叶页语法（物理池化输入格式：布局、派生宽度、池化 body、inline/root 两种元数据形态）；strata 不编码 inode 页 |
| layerfs-content/tests/inode_leaf.rs::a_pooled_body_keeps_the_prefix_and_the_serials | 池化 body 保留前缀与各 serial | — | 不保留 | 同上 |
| layerfs-content/tests/inode_leaf.rs::an_inode_value_round_trips_and_rejects_a_foreign_kind | inode 值往返，并拒绝外来的 kind | — | 不保留 | 同上 |
| layerfs-content/tests/inode_leaf.rs::a_pooled_value_object_is_exactly_ninety_four_canonical_bytes | 池化的值对象恰为 94 个 canonical 字节；损坏的魔数是语法失败，而翻转 payload 字节由对象身份拒绝 | — | 不保留 | 同上 |
| layerfs-content/tests/inode_leaf.rs::malformed_leaves_are_rejected | 畸形叶页被拒：尾随字节、错误的 role 标签、计数与子树计数不一致 | — | 不保留 | 同上 |
| layerfs-content/tests/inode_leaf.rs::a_pooled_body_rejects_a_zero_ordinal_and_a_wrong_width | 池化 body 拒绝零序号与错误宽度 | — | 不保留 | 同上 |
| layerfs-content/tests/inode_leaf.rs::checked_pooling_leaf_reuses_decode_and_retains_all_ordinal_checks | 经检查的池化叶复用解码并保留全部序号检查 | — | 不保留 | 同上 |
| layerfs-content/tests/inode_leaf.rs::the_inline_form_encodes_and_decodes_at_its_exact_offsets | inline 形态在精确偏移处编码与解码 | — | 不保留 | 同上 |
| layerfs-content/tests/inode_leaf.rs::the_root_form_stays_byte_identical_to_the_old_layout | root 形态与旧布局逐字节相同 | — | 不保留 | 同上 |
| layerfs-content/tests/inode_leaf.rs::a_leaf_round_trips_with_both_metadata_forms | 叶页在两种元数据形态下都往返 | — | 不保留 | 同上 |
| layerfs-content/tests/inode_leaf.rs::the_inline_form_rejects_bad_padding_bad_kind_and_bad_values | inline 形态拒绝非零填充、外来 kind 与非法的符号链接 mode/纳秒值 | — | 不保留 | 同上 |
| layerfs-content/tests/inode_leaf.rs::a_pooled_value_wraps_the_inline_form | 池化值包裹 inline 形态 | — | 不保留 | 同上 |
| layerfs-content/tests/merge_units.rs::malformed_patterns_and_bad_names_are_refused | 合并单元的非法 pattern（空、/、#、a//b、含换行、无 pattern、以 ! 开头）与非法或重名的单元名被拒；带否定的合法单元与空集合被接受 | — | 不保留 | 实现细节（merge unit）：LayerFS 的目录级合并单元（按名字分组的 pattern 集合）；strata v1 只有文件级 .lww，没有合并单元 |
| layerfs-content/tests/merge_units.rs::a_unit_covers_its_matches_and_below_and_the_first_listed_wins | 单元覆盖其匹配项与其下全部路径，多个单元命中时列在前面的优先，否定 pattern 清除其下匹配 | — | 不保留 | 同上 |
| layerfs-content/tests/merge_units.rs::a_relink_that_rewrote_nothing_is_not_a_change | 两侧目录里文件完全相同、只是 inode serial 因分配顺序不同而不同时，不算改动 | overlay | 改写 | 改写为 overlay::compute 的内容判定用例：按内容哈希判定，重建或 inode/gen count 变化但内容相同的文件不算改动 |
| layerfs-content/tests/merge_units.rs::only_units_both_sides_changed_are_decided_and_ours_wins_a_tie | 只有两边都改了的单元才需要决议：较新的一边保留，时间相同时 ours 胜；只一边改的单元直接采用 | publish | 改写 | 改写为 publish 的 .lww 决议用例：只有两个会话都改了的 .lww 文件才需决议，较新 checkpoint 胜出；时间相同时的取舍由 publish 文档规定；'合并单元'粒度不保留 |
| layerfs-content/tests/object_admission.rs::all_thirteen_roles_derive_exact_references_and_move_the_canonical_allocation | 十三种 role 都能推导出精确的直接引用，并且 canonical 分配不经拷贝移动 | — | 不保留 | 实现细节（对象信封）：FinalizedObject::admit 对不可信 ID/role/字节的语义准入；strata 的 put_blob 直接哈希文件字节，没有 role 与信封 |
| layerfs-content/tests/object_admission.rs::role_confusion_truncation_and_generic_envelopes_are_refused | role 混淆、截断与通用信封被拒 | — | 不保留 | 同上 |
| layerfs-content/tests/object_admission.rs::domain_identity_scope_and_selected_store_policy_are_required | 域、身份、scope 与所选 Store policy 都是必需的 | — | 不保留 | 同上 |
| layerfs-content/tests/object_admission.rs::rehashed_impossible_counts_summaries_flags_and_values_are_refused | 重算哈希后仍不可能的计数、摘要、flag 与值被拒 | — | 不保留 | 同上 |
| layerfs-content/tests/object_context.rs::all_thirteen_roles_validate_with_direct_children_and_scoped_one_id_demands | 十三种 role 都用直接子对象与限定 scope 的单 ID 请求完成上下文校验 | — | 不保留 | 实现细节（对象信封）：validate_context 对直接子对象与摘要的上下文校验；strata 的 blob 之间没有父子引用 |
| layerfs-content/tests/object_context.rs::local_reference_and_identity_context_refusals_precede_all_demands | 本地引用与身份上下文的拒绝都发生在任何请求之前 | — | 不保留 | 同上 |
| layerfs-content/tests/object_context.rs::extent_payload_kind_slice_and_file_state_actual_summaries_are_checked | extent 的 payload 类型、切片与 file state 的实际摘要被检查 | — | 不保留 | 同上 |
| layerfs-content/tests/object_context.rs::extent_branches_check_nonroot_fill_level_and_exact_cumulative_summaries | extent branch 检查非根页的填充、层级与精确的累计摘要 | — | 不保留 | 同上 |
| layerfs-content/tests/object_context.rs::inode_placement_content_kind_and_portable_metadata_are_contextual | inode 的位置、内容类型与 portable 元数据在上下文中校验 | — | 不保留 | 同上 |
| layerfs-content/tests/object_context.rs::regular_roots_obey_whole_file_capacity_and_retain_small_file_states | 普通文件根遵守整文件容量，并保留小文件的 file state | — | 不保留 | 同上 |
| layerfs-content/tests/object_context.rs::filesystem_root_requires_the_expected_zero_count_directory_record | 文件系统根要求预期的零计数目录记录 | — | 不保留 | 同上 |
| layerfs-content/tests/object_context.rs::failures_and_cardinality_end_the_original_demand_without_retry | 失败与基数错误终止原始请求，不重试 | — | 不保留 | 同上 |
| layerfs-content/tests/object_context.rs::a_hundred_large_inode_roots_do_not_form_one_oversized_provider_batch | 一百个大 inode 根不会组成一个超大的 provider 批次 | — | 不保留 | 同上 |
| layerfs-content/tests/object_context.rs::inode_branches_check_fill_level_maximum_count_and_leaf_ranges | inode branch 检查填充、层级、最大计数与叶子范围 | — | 不保留 | 同上 |
| layerfs-content/tests/object_context.rs::directory_branches_check_fill_level_maximum_and_exact_counts_and_bytes | 目录 branch 检查填充、层级、最大值与精确的计数和字节数 | — | 不保留 | 同上 |
| layerfs-content/tests/object_context.rs::attribute_branches_check_fill_level_maximum_and_exact_counts_and_bytes | 属性 branch 检查填充、层级、最大值与精确的计数和字节数 | — | 不保留 | 同上 |
| layerfs-content/tests/object_context.rs::attribute_values_require_extent_roots_and_refuse_oversized_lengths_before_mapping_demand | 属性值要求 extent 根，超长长度在映射请求之前就被拒 | — | 不保留 | 同上 |
| layerfs-content/tests/object_identity.rs::identity_is_domain_separated_and_not_a_raw_digest | ObjectId 是带域前缀的 BLAKE3，不等于裸摘要，也不等于旧版摘要 | — | 不保留 | 实现细节（对象信封）：ObjectId 的域分隔（layerfs/object/v2）；strata 的 BlobId 就是文件内容的裸 BLAKE3 |
| layerfs-content/tests/object_identity.rs::identity_has_no_variable_width_or_text_shortcuts | 标识固定 32 字节：from_bytes 拒绝其他长度，文本形式往返一致，非法文本被拒 | types | 改写 | 改写为 types::BlobId 的 TryFrom<String>/Display 用例：64 位小写十六进制（BLAKE3），拒绝错长度与非十六进制文本（返回 Code::InvalidInput），往返一致 |
| layerfs-content/tests/object_identity.rs::small_fixture_bytes_and_identity_are_frozen | 小文件固件的 canonical 字节与标识被冻结 | — | 不保留 | 实现细节（对象信封）：canonical framing、role 与字段限额；strata 的 blob 就是原始文件字节 |
| layerfs-content/tests/object_identity.rs::chunk_fixture_bytes_and_identity_are_frozen | chunk 固件的 canonical 字节与标识被冻结 | — | 不保留 | 实现细节（分块编码）：chunk 对象的冻结固件；strata 不切块 |
| layerfs-content/tests/object_identity.rs::mapping_profile_and_empty_form_identities_are_frozen | 映射 profile 与空文件形态的标识被冻结 | — | 不保留 | 实现细节（映射页）：映射 profile 与空叶页的冻结固件；strata 无映射树 |
| layerfs-content/tests/object_identity.rs::malformed_envelopes_are_rejected_exactly | 畸形信封被精确拒绝（EOF、尾随字节、未知 framing、role 不符、超限） | — | 不保留 | 实现细节（对象信封）：canonical framing、role 与字段限额；strata 的 blob 就是原始文件字节 |
| layerfs-content/tests/object_identity.rs::writing_to_a_sink_matches_the_single_allocation_encoder | 写入 sink 的字节与单次分配编码器的结果一致 | — | 不保留 | 同上 |
| layerfs-content/tests/object_identity.rs::identity_of_a_damaged_canonical_object_changes | 被破坏的 canonical 对象得到不同的标识 | store | 改写 | 改写为 store::BlobId::of：任意一个字节的改动都得到不同 BlobId，内容寻址据此发现损坏 |
| layerfs-content/tests/object_identity.rs::the_field_ceiling_is_the_frozen_eight_mib_not_the_envelope | 字段上限冻结为 8 MiB，低于 16 MiB 的信封上限；超限在编码与解码两侧都被拒 | — | 不保留 | 实现细节（对象信封）：canonical framing、role 与字段限额；strata 的 blob 就是原始文件字节 |
| layerfs-content/tests/object_identity.rs::the_envelope_ceiling_is_enforced_at_plus_or_minus_one | 信封上限在 ±1 字节处被精确执行，超限对象到不了 Store | — | 不保留 | 同上 |
| layerfs-content/tests/object_identity.rs::moving_an_objects_pieces_keeps_its_advisory_predecessors | into_parts 取走对象各部分时保留其 advisory predecessor | — | 不保留 | 实现细节（delta 编码）：对象携带的 predecessor 提示；strata 不做 delta |
| layerfs-content/tests/runs.rs::run_roots_equal_streaming_at_cutoff_cdc_and_leaf_partition_boundaries | run 构造的根在 cutoff、CDC 与叶页划分边界处等于流式构造 | — | 不保留 | 实现细节（分块编码）：data/zero runs 构造与流式构造的等价（含 TB 级零 run 与切块边界）；strata 不切块，也不构造稀疏 run |
| layerfs-content/tests/runs.rs::multi_level_repetition_matches_streaming_extent_builder_without_zero_byte_scan | 多层重复与流式 extent 构造器一致，且不做零字节扫描 | — | 不保留 | 同上 |
| layerfs-content/tests/runs.rs::terabyte_zero_runs_emit_logarithmic_objects_and_read_exact_boundaries | TB 级零 run 只产出对数个对象，并精确读回边界字节 | — | 不保留 | 同上 |
| layerfs-content/tests/runs.rs::invalid_windows_and_consumer_failure_stop_the_single_operation | 非法窗口与消费者失败使单次操作停止 | — | 不保留 | 同上 |
| layerfs-content/tests/runs.rs::short_repeated_tail_preserves_mixed_unflushed_extents | 较短的重复尾部保留混合的未 flush extent | — | 不保留 | 同上 |
| layerfs-content/tests/streaming.rs::input_requests_stay_bounded_by_the_declared_windows | 每次读取都不超过声明的窗口，来源恰好交付一遍输入，没有字节被读两次 | — | 不保留 | 实现细节（性能/资源预算）：流式构造的读取窗口、保留条目与分配；strata 的 put_blob 不流式读入内存 |
| layerfs-content/tests/streaming.rs::a_short_source_is_a_definitive_whole_file_result | 来源短于 cutoff 时直接得到整文件结果 | — | 不保留 | 实现细节（分块编码）：cutoff 探测；strata 不切块 |
| layerfs-content/tests/streaming.rs::children_are_emitted_before_their_parents | 产出顺序是子对象先于父对象（每个被引用的对象在引用它的对象之前发出） | store | 改写 | 改写为 store 的引用完整性用例：checkpoint 行提交之前，它引用的 blob 已全部落盘，不存在悬空引用 |
| layerfs-content/tests/streaming.rs::many_files_do_not_retain_the_whole_workload | 一个流保留的东西只取决于该流本身，而不取决于此前处理了多少文件 | — | 不保留 | 实现细节（性能/资源预算）：流式构造的读取窗口、保留条目与分配；strata 的 put_blob 不流式读入内存 |
| layerfs-content/tests/streaming.rs::a_late_input_failure_is_returned_once_after_early_output | 较晚的输入失败只返回一次，先前已发出的对象留存 | — | 不保留 | 实现细节（分块编码）：流式构造中途失败时已发出的 chunk 留存；strata 的 put_blob 要么整文件落盘、要么不落盘 |
| layerfs-content/tests/streaming.rs::unknown_length_and_known_length_agree_object_by_object | 长度未知与长度已知两种输入，对同一字节得到逐对象相同的结果 | store | 改写 | 改写为 store::BlobId：同一字节无论一次读入还是分块流式哈希，BlobId 相同 |
| layerfs-content/tests/streaming.rs::a_discarding_consumer_never_allocates_a_payload_copy | 丢弃型 consumer 从不分配 payload 副本 | — | 不保留 | 实现细节（性能/资源预算）：流式构造的读取窗口、保留条目与分配；strata 的 put_blob 不流式读入内存 |
| layerfs-content/tests/streaming.rs::root_identity_is_stable_across_repeated_streaming_runs | 对同一输入重复流式构造，根的身份稳定 | store | 改写 | 改写为 store::put_blob 的幂等用例：同一内容重复写入得到同一 BlobId |
| layerfs-content/tests/streaming.rs::the_builder_retains_a_bounded_number_of_entries | 构造器保留的解码条目数只取决于层高与页容量，而不取决于文件长度 | — | 不保留 | 实现细节（性能/资源预算）：流式构造的读取窗口、保留条目与分配；strata 的 put_blob 不流式读入内存 |
| layerfs-content/tests/timing.rs::small_construction_reports_its_real_scopes | 小文件构造上报其真实的计时 scope | — | 不保留 | 实现细节（性能/资源预算）：构造与读取的计时 scope 与开关等价；strata 的验收数字另在各模块 §7 |
| layerfs-content/tests/timing.rs::chunked_construction_reports_the_chunk_scope | 分块构造上报 chunk scope | — | 不保留 | 同上 |
| layerfs-content/tests/timing.rs::recording_and_disabled_execution_produce_identical_results | 开启与关闭计时记录，执行结果完全相同 | — | 不保留 | 同上 |
| layerfs-content/tests/timing.rs::a_construction_failure_keeps_its_completed_timing | 构造失败时保留已完成部分的计时 | — | 不保留 | 同上 |
| layerfs-content/tests/timing.rs::a_read_reports_its_own_scopes_without_storage | 读取在不涉及存储的情况下上报自己的 scope | — | 不保留 | 同上 |
| layerfs-content/tests/timing.rs::a_discarding_consumer_still_runs_the_real_constructor | 丢弃型 consumer 仍然运行真实的构造器 | — | 不保留 | 同上 |
| layerfs-content/tests/timing.rs::repeated_labels_stay_distinct_and_ordered | 重复的标签保持区分并有序 | — | 不保留 | 同上 |
| layerfs-content/tests/zero_cdc.rs::frozen_zero_chunk_period_is_the_maximum | 全零输入的 chunk 周期冻结为最大 chunk 长度 | — | 不保留 | 实现细节（分块编码）：全零输入的 GEAR 切块周期；strata 不切块 |
| layerfs-storage/tests/codec_frames.rs::the_decode_arena_is_charged_only_when_a_frame_needs_it | 解码 arena 在帧需要时才计费（首次使用时分配，第二帧复用），而不是在读取打开时 | — | 不保留 | 实现细节（压缩 codec）：zstd 帧在 FFI 边界上的解码与恶意帧拒绝；strata 的 blob 是原始文件字节，不压缩、没有 unsafe 编解码边界 |
| layerfs-storage/tests/codec_frames.rs::a_valid_frame_round_trips_through_both_decode_entry_points | 合法帧经两个解码入口都往返；prefix 入口借用 base 作为原始 Zstandard 前缀，调用后引用即清除 | — | 不保留 | 同上 |
| layerfs-storage/tests/codec_frames.rs::a_declared_length_that_contradicts_the_frame_is_refused | 声明长度与帧内容矛盾被拒 | — | 不保留 | 同上 |
| layerfs-storage/tests/codec_frames.rs::a_forged_declared_length_inside_the_header_is_refused_by_the_call_itself | 帧头被改成与调用方声称的同一个错误长度时，由 FFI 调用或事后长度检查拒绝，目的缓冲区不会溢出 | — | 不保留 | 同上 |
| layerfs-storage/tests/codec_frames.rs::every_truncation_of_a_valid_frame_is_refused_without_panicking | 合法帧的任意截断都被拒，且不 panic | — | 不保留 | 同上 |
| layerfs-storage/tests/codec_frames.rs::a_frame_whose_checksum_no_longer_matches_is_refused | 校验和不再匹配的帧被拒 | — | 不保留 | 同上 |
| layerfs-storage/tests/codec_frames.rs::a_frame_beyond_the_profiles_window_is_refused | 超出 profile 窗口（20 对 18）的帧被拒，且拒绝并非来自帧长上限 | — | 不保留 | 同上 |
| layerfs-storage/tests/concurrent_winner_chain.rs::a_concurrent_winner_chain_resolves_a_base_this_wave_located_absent | 并发写入者先注册了同内容的对象时，落败的 Save 要解析胜者的整条依赖链，包括本 Save 在这一波里判为不存在、之后被别人发布的 base | — | 不保留 | 实现细节（delta 编码）：胜者的 PREFIX 依赖链解析与 first-wins locator 注册（F-036）；strata 无依赖链。并发写同一内容的幂等性见 persistence 的 concurrent_first_installs 行 |
| layerfs-storage/tests/decoded_group_retention.rs::overflow_keeps_recent_dependency_groups_and_evicts_only_a_cold_group | 解码后的依赖组溢出时保留最近使用的、只淘汰冷组 | — | 不保留 | 实现细节（delta 编码）：解码后依赖组的保留（LRU）；strata 无依赖组 |
| layerfs-storage/tests/decoded_group_retention.rs::duplicate_input_is_idempotent_and_invalid_admission_preserves_retention | 重复输入是幂等的，无效准入不改变保留集合 | — | 不保留 | 同上 |
| layerfs-storage/tests/delta_depth_counts.rs::a_chain_stops_at_the_depth_cap_and_reports_it | Save 报告每个 payload 对象的链深度：在 depth 上限处停止并报告原因 | — | 不保留 | 实现细节（delta 编码）：PREFIX 链深度上限与拒绝原因计数 |
| layerfs-storage/tests/delta_depth_counts.rs::a_chain_stops_at_the_work_budget_before_a_deep_cap | 链在达到深度上限之前就受 512 KiB 的 canonical 链预算限制，并报告该原因 | — | 不保留 | 同上 |
| layerfs-storage/tests/encode_region_reuse.rs::reused_and_poisoned_encode_regions_give_byte_identical_frames | 被复用、被毒化的编码区域产出逐字节相同的帧 | — | 不保留 | 实现细节（压缩 codec）：encode/decode 工作区的复用与毒化，不影响输出（F-043）；strata 不压缩 |
| layerfs-storage/tests/encode_region_reuse.rs::a_region_of_another_size_is_refused | 尺寸不符的区域被拒 | — | 不保留 | 同上 |
| layerfs-storage/tests/encode_region_reuse.rs::reused_and_poisoned_decode_regions_give_byte_identical_output | 被复用、被毒化的解码区域产出逐字节相同的输出 | — | 不保留 | 同上 |
| layerfs-storage/tests/exact_reuse_trust.rs::an_exact_reuse_reads_no_stored_bytes | Save 提供 Store 里已有的对象（同 role、同 canonical 长度）时，计为复用（inserted 0 / reused 1），不回读已存副本 | store | 改写 | 改写为 store::put_blob 的幂等用例（blob_put_is_idempotent）：同一 BlobId 已存在时不新增 blob，也不回读已存副本 |
| layerfs-storage/tests/exact_reuse_trust.rs::a_role_or_length_mismatch_is_still_refused_at_save | 同一身份下 role 或长度与已存对象不符时，Save 仍然拒绝（Collision） | store | 改写 | 改写为 store::put_blob 的长度核对用例：已存同名 blob 的长度与来源不符 -> 拒绝，不静默采纳；role 一项不保留（blob 无 role）；types 的 Code 没有专门的完整性码，归入 Unknown 还是新增由 store 文档决定 |
| layerfs-storage/tests/exact_reuse_trust.rs::a_corrupt_stored_copy_is_accepted_at_save_and_fails_at_its_next_read | 已存副本在写入后被损坏：复用它的 Save 照常接受（不回读），其后的读取因内容与 id 不符而认证失败 | store | 改写 | 改写为 store 的完整性用例：put 复用已存 blob 时不回读校验；已被损坏的 blob 在之后的 get_blob 因 BLAKE3 与 BlobId 不符而失败（错误码归类同 filesystem_failure::a_malformed_or_unauthenticated… 行） |
| layerfs-storage/tests/exact_reuse_trust.rs::an_exact_reuse_of_a_pooled_leaf_acquires_no_pack | 复用池化的元数据叶页同样不取任何 pack，其值不重建，叶页仍可完整读回 | — | 不保留 | 实现细节（inode 值池化）：池化 inode 叶页的复用；strata 无池化叶页 |
| layerfs-storage/tests/inode_branch_deltas.rs::branch_chains_grow_to_the_cap_reset_whole_and_read_back | branch 链长从 1 长到上限后回到整页存储，新旧表都逐值读回 | — | 不保留 | 实现细节（delta 编码）：inode 表 branch 页对其前驱页的 delta 存储（链长 1..8、到上限回到整页、旧 Store 兼容）；strata 无 inode 表 |
| layerfs-storage/tests/inode_branch_deltas.rs::a_store_of_whole_branches_reads_identically_after_delta_branches_join_it | 只含整页 branch 的旧 Store 在 delta branch 加入之后读取不变 | — | 不保留 | 同上 |
| layerfs-storage/tests/inode_branch_deltas.rs::a_lookup_reconstructs_one_branch_chain_per_level | 批量查找对每个访问到的 branch 页只重建一条链 | — | 不保留 | 同上 |
| layerfs-storage/tests/inode_branch_deltas.rs::a_refused_base_stores_the_branch_whole_and_it_reads_back | 前驱未存储时 branch 页被整页存储且可读回；同一页再次提供是精确复用，不是新记录 | — | 不保留 | 同上 |
| layerfs-storage/tests/inode_branch_merge_reads.rs::a_merge_reconstructs_one_branch_chain_per_demanded_branch_page | 合并（ours、theirs 各改一个文件）读取 delta 存储的 inode branch 时，每个被请求的 branch 页只重建一条链；合并结果含双方改动 | — | 不保留 | 实现细节（delta 编码）：inode 表 branch 页对其前驱页的 delta 存储（链长 1..8、到上限回到整页、旧 Store 兼容）；strata 无 inode 表 |
| layerfs-storage/tests/lazy_candidate_ring.rs::a_lazily_loaded_ring_selects_what_a_loaded_ring_selects | delta 候选环在首次使用时才加载，其选择与 Save 开始前就已加载的环相同 | — | 不保留 | 实现细节（delta 编码）：delta 候选环的惰性加载；strata 不选 delta 候选 |
| layerfs-storage/tests/locator_pressure.rs::pressure_admission_preserves_an_unrequested_positive_locator | 压力下的准入保留未被请求的正向 locator | — | 不保留 | 实现细节（SQLite 包布局）：locator 缓存在 4096 项上界下的保留与淘汰；strata 没有 locator 表 |
| layerfs-storage/tests/locator_pressure.rs::second_chance_breaks_repeated_low_key_victim_churn | second chance 打破反复低 key 受害者的抖动 | — | 不保留 | 同上 |
| layerfs-storage/tests/logical_size_lengths.rs::catalogue_lengths_give_the_payload_route_size_for_every_representation | 对各种表示的文件（空、几字节、整文件、分块、硬链接、已删、已换），按 catalogue 长度算出的大小等于逐文件 payload 路线的大小 | — | 不保留 | 实现细节（文件系统树编码）：由 catalogue 长度推导 Commit 的逻辑大小（窗口化查询）；strata 不计算树的逻辑大小 |
| layerfs-storage/tests/logical_size_lengths.rs::more_changed_files_than_one_window_are_asked_in_several_demands | 变更文件数超过一个 LENGTH_WINDOW 时分多次请求 | — | 不保留 | 同上 |
| layerfs-storage/tests/object_key.rs::sealed_keys_keep_standard_sha256_bytes_for_empty_short_and_many_blocks | 封存键保持标准 SHA-256 字节：空串、短串（abc）、一百万个 a 的已知答案 | store | 改写 | 改写为 store 的 BlobId 已知答案用例：空串、短串与跨多块输入的 BLAKE3 摘要向量固定，保护 BlobId 的字节兼容（算法由 SHA-256 换成 BLAKE3，向量需重算） |
| layerfs-storage/tests/pack_cache_contract.rs::pressure_evicts_only_cold_bodies_and_keeps_recent_siblings | 压力下只淘汰冷 body，保留近期的兄弟 body | — | 不保留 | 实现细节（SQLite 包布局）：pack 体缓存的容量释放、cohort 保护与 owner 生命周期；strata 的 blob 是普通文件，没有 pack 缓存 |
| layerfs-storage/tests/pack_cache_contract.rs::cohort_reservation_keeps_requested_hits_while_releasing_misses_space | cohort 预留保住被请求的命中项，同时为未命中项腾出空间 | — | 不保留 | 同上 |
| layerfs-storage/tests/pack_cache_contract.rs::singleton_is_isolated_and_new_owner_has_no_retained_input | singleton 与其他项隔离，新 owner 没有遗留输入 | — | 不保留 | 同上 |
| layerfs-storage/tests/pack_range_contract.rs::ranges_crossing_prefix_and_scan_chunks_are_exact_and_all_bytes_are_paid_once | 跨越前缀与扫描块的范围读取精确，所有字节只计费一次 | — | 不保留 | 实现细节（SQLite 包布局）：从 pack 里取选定字节范围，且必须来自一次完整的已认证扫描；strata 无 pack |
| layerfs-storage/tests/pack_range_contract.rs::corruption_outside_selected_ranges_is_still_refused | 选定范围之外的损坏仍然被拒 | — | 不保留 | 同上 |
| layerfs-storage/tests/pack_range_contract.rs::invalid_ranges_fail_without_body_scan_or_whole_strategy_fallback | 非法范围直接失败，不做 body 扫描，也不回退到整体策略 | — | 不保留 | 同上 |
| layerfs-storage/tests/pack_range_contract.rs::whole_selection_keeps_the_existing_carrier_and_io_failure_is_terminal | 整体选择沿用既有载体，I/O 失败是终局 | — | 不保留 | 同上 |
| layerfs-storage/tests/physical_formats.rs::grouped_whole_file_versions_keep_their_own_body_offsets | grouped whole-file 各版本保持各自的 body 偏移 | — | 不保留 | 实现细节（SQLite 包布局）：pack 的 framing 版本、lane 与目录布局；strata 的 blob 是普通文件，没有 framing |
| layerfs-storage/tests/physical_formats.rs::ordinary_and_chunk_versions_keep_their_own_body_offsets | ordinary 与 chunk 各版本保持各自的 body 偏移 | — | 不保留 | 同上 |
| layerfs-storage/tests/physical_formats.rs::every_implemented_framing_is_recognized_by_its_own_version | 每种已实现的 framing 都按自己的版本被识别，pack 声明长度等于实际长度 | — | 不保留 | 同上 |
| layerfs-storage/tests/physical_formats.rs::unimplemented_and_unknown_framings_are_rejected_without_a_trial_decode | 未实现或未知的 framing 版本被具名拒绝，不做试探性解码；已识别版本的控制区畸形是完整性失败 | — | 不保留 | 同上 |
| layerfs-storage/tests/physical_formats.rs::declared_pack_and_group_bounds_are_enforced_on_the_bytes | 声明的 pack 与 group 上限（256 KiB、singleton 16 MiB+4096、零组非法）在字节上被执行 | — | 不保留 | 同上 |
| layerfs-storage/tests/physical_formats.rs::the_lane_table_names_one_limit_per_lane | lane 表为每条 lane 指明一个上限 | — | 不保留 | 同上 |
| layerfs-storage/tests/physical_formats.rs::a_recipe_frame_larger_than_its_profile_is_refused | 超出 profile 的 recipe 帧被具名的容量错误拒绝，而不是截断 | — | 不保留 | 实现细节（压缩 codec）：codec profile 的 raw/frame 上限；strata 不压缩 |
| layerfs-storage/tests/physical_formats.rs::closing_a_pack_and_retaining_its_tail_assemble_the_same_bytes | 关闭 pack 与保留尾部两条路径产出逐字节相同的 pack | — | 不保留 | 实现细节（SQLite 包布局）：pack 的 framing 版本、lane 与目录布局；strata 的 blob 是普通文件，没有 framing |
| layerfs-storage/tests/physical_formats.rs::written_packs_size_their_directory_to_their_groups | 写出的 pack 目录恰好按其 group 数定长，不留保留字节 | — | 不保留 | 同上 |
| layerfs-storage/tests/pooled_demand_work.rs::pooled_demand_reuses_one_checked_physical_reconstruction | 池化请求只重建一次物理叶页并复用已检查的重建结果，序号预取不让工作翻倍 | — | 不保留 | 实现细节（inode 值池化）：池化叶页的物理重建次数 |
| layerfs-storage/tests/pooled_demand_work.rs::a_missing_stored_value_still_refuses_canonical_reconstruction | 缺失的已存值仍然拒绝 canonical 重建 | — | 不保留 | 同上 |
| layerfs-storage/tests/pooled_dependency_integrity.rs::literal_delta_cannot_hide_a_wrong_canonical_base | 合法的依赖项不能掩盖池化链里被损坏的 canonical base（链上每个元素都按自己的身份校验） | — | 不保留 | 实现细节（delta 编码）：依赖链上每个元素的身份校验；strata 无依赖链，读取校验见 exact_reuse_trust 的改写行 |
| layerfs-storage/tests/read_demand_custody.rs::mixed_hit_miss_wave_keeps_all_requested_locators_within_existing_bound | 命中与未命中混合的读取 wave 在既有上界内保留全部被请求的 locator | — | 不保留 | 实现细节（SQLite 包布局）：locator 缓存在 4096 项上界下的保留与淘汰；strata 没有 locator 表 |
| layerfs-storage/tests/read_pack_locality.rs::bounded_hash_order_demand_does_not_reacquire_one_pack_for_each_object | 有界的哈希序请求不会为每个对象重新取同一个 pack（依赖检查一趟、重建一趟） | — | 不保留 | 实现细节（acquisition profile）：读取时按 pack 局部性调度，避免为每个对象重取同一个 pack |
| layerfs-storage/tests/read_pack_locality.rs::physical_scheduling_preserves_repeated_ids_and_demand_order | 物理调度保留重复 id 与请求顺序 | — | 不保留 | 同上 |
| layerfs-storage/tests/save_demand_locality.rs::exact_reuse_does_not_scan_the_wave_before_consuming_its_objects | 精确复用不会在消费对象之前先扫描整个 wave | — | 不保留 | 实现细节（acquisition profile）：Save 时对有界物理输入的消费顺序 |
| layerfs-storage/tests/save_encoder_reuse.rs::saves_in_turn_on_a_thread_pool_do_not_keep_a_workspace_per_thread | 线程池上轮流进行的 Save 共用一个编码工作区，而不是每个线程各留一个 | — | 不保留 | 实现细节（压缩 codec）：编码工作区按线程池共享（F-043）；strata 不压缩 |
| layerfs-storage/tests/save_resolved_reads.rs::a_save_reads_an_object_it_resolved_without_acquiring_its_pack_again | Save 再次读取自己已解析过的对象时不再取 pack | — | 不保留 | 实现细节（读缓存）：Save 内的已解析 canonical 缓存（SAVE_RESOLVED_CACHE_BYTES）；strata 直接读文件，不缓存解析结果 |
| layerfs-storage/tests/save_resolved_reads.rs::past_its_bound_the_resolved_cache_releases_the_least_recently_read | 超过上界后已解析缓存释放最久未读的对象，每次读取仍返回对象的字节 | — | 不保留 | 同上 |
| layerfs-storage/tests/selected_cache_bounds.rs::selected_units_evict_selectively_under_the_same_two_mib_body_limit | 选定单元在同一 2 MiB body 上限下被选择性淘汰 | — | 不保留 | 实现细节（SQLite 包布局）：选定单元与整体 body 共用同一 2 MiB 字节/条数上界的缓存；strata 无 pack 缓存 |
| layerfs-storage/tests/selected_cache_bounds.rs::tiny_selected_units_still_obey_the_existing_entry_count_limit | 很小的选定单元仍遵守既有的条目数上限 | — | 不保留 | 同上 |
| layerfs-storage/tests/selected_cache_bounds.rs::whole_promotion_replaces_units_within_same_byte_allowance | 整体提升在同一字节额度内替换单元 | — | 不保留 | 同上 |
| layerfs-storage/tests/selected_cache_bounds.rs::whole_cohort_shares_full_directory_walks_and_preserves_descriptor_checks | 整体 cohort 共享完整的目录遍历，并保留描述符检查 | — | 不保留 | 同上 |
| layerfs-storage/tests/selected_cache_bounds.rs::cohort_still_refuses_corruption_in_an_unrequested_directory_entry | cohort 仍然拒绝未被请求的目录项里的损坏 | — | 不保留 | 同上 |
| layerfs-storage/tests/selected_group_reads.rs::adjacent_sparse_groups_coalesce_and_dense_promotion_preserves_bytes_and_order | 相邻的稀疏 group 合并，密集提升后字节与顺序保持不变 | — | 不保留 | 实现细节（SQLite 包布局）：稀疏/密集 group 的整体与范围读取策略、合并、提升与复用；strata 无 pack |
| layerfs-storage/tests/selected_group_reads.rs::new_sparse_group_miss_promotes_retained_pack_then_all_siblings_reuse | 新的稀疏 group 未命中会提升已保留的 pack，其后所有兄弟 group 复用 | — | 不保留 | 同上 |
| layerfs-storage/tests/selected_group_reads.rs::random_sparse_groups_preserve_duplicate_slots | 随机稀疏 group 保留重复的槽位 | — | 不保留 | 同上 |
| layerfs-storage/tests/selected_group_reads.rs::unread_corruption_is_outside_scoped_read_but_whole_pack_audit_refuses_it | 未被读到的损坏不影响范围读取，但整 pack 审计会拒绝它 | — | 不保留 | 实现细节（SQLite 包布局）：整 pack 审计；strata 没有 pack，也没有 Store 级 fsck |
| layerfs-storage/tests/selected_group_reads.rs::accessed_record_corruption_fails_canonical_authentication | 被访问的记录被损坏时，读取在 canonical 认证处失败 | store | 改写 | 改写为 store::get_blob 的完整性用例：blob 内容被改动一个比特 -> 读取因 BLAKE3 与 BlobId 不符而失败 |
| layerfs-storage/tests/selected_group_reads.rs::hash_valid_malformed_directory_is_refused_by_the_shared_prefix_validator | 哈希合法但目录畸形的 pack 被共享的前缀校验器拒绝 | — | 不保留 | 实现细节（SQLite 包布局）：稀疏/密集 group 的整体与范围读取策略、合并、提升与复用；strata 无 pack |
| layerfs-storage/tests/selected_group_reads.rs::a_sparse_prefix_target_reconstructs_its_required_base_exactly | 稀疏的 PREFIX 目标精确重建其所需的 base | — | 不保留 | 实现细节（delta 编码）：稀疏 PREFIX 记录的 base 重建 |
| layerfs-storage/tests/selected_group_reads.rs::sparse_chunk_records_reconstruct_canonical_chunk_bytes | 稀疏的 chunk 记录重建出规范的 chunk 字节 | — | 不保留 | 实现细节（分块编码）：chunk 记录的稀疏重建 |
| layerfs-storage/tests/selected_group_reads.rs::sparse_payload_siblings_survive_multiple_pack_pressure_without_whole_promotion | 多个 pack 压力下稀疏的 payload 兄弟不被整体提升也能存活 | — | 不保留 | 实现细节（SQLite 包布局）：稀疏/密集 group 的整体与范围读取策略、合并、提升与复用；strata 无 pack |
| layerfs-storage/tests/selected_group_reads.rs::dense_payload_coverage_promotes_once_and_then_reuses_sibling_groups | 密集的 payload 覆盖只提升一次，其后兄弟 group 复用 | — | 不保留 | 同上 |
| layerfs-storage/tests/stored_value_reads.rs::a_changed_leaf_stores_one_row_for_each_changed_value | 叶页中每个改变的值只存一行；一次值查找解析整条链的序号 | — | 不保留 | 实现细节（inode 值池化）：inode 值作为按序号存储的行、池化叶页的读写工作量；strata 无 inode 值行 |
| layerfs-storage/tests/stored_value_reads.rs::a_first_save_reads_nothing_of_the_history_it_does_not_change | 没有保存过任何东西的 handle 保存一个无关的新叶页时，不取任何早先的 pack，也不查任何已存值 | — | 不保留 | 同上 |
| layerfs-storage/tests/stored_value_reads.rs::one_save_stores_a_repeated_value_once | 一次 Save 内重复的值只存一次（解包依赖树里相同的文件） | — | 不保留 | 同上 |
| layerfs-storage/tests/stored_value_reads.rs::a_wrong_or_absent_stored_value_fails_the_read | 错误或缺失的已存值使读取失败，链上每个元素都按自己的身份校验 | — | 不保留 | 同上 |
| layerfs-storage/tests/stored_value_reads.rs::one_handle_reads_any_number_of_leaves_at_one_lookup_each | 一个 handle 读取任意多个叶页，每个叶页一次查找 | — | 不保留 | 同上 |
| layerfs-storage/tests/verified_pack_read.rs::immutable_carrier_rejects_wrong_length_digest_and_modified_consumed_bytes | 不可变载体拒绝错误的长度、摘要与被消费后又被改动的字节 | — | 不保留 | 实现细节（SQLite 包布局）：pack 读取的不可变载体（长度/摘要/消费后改动）与 C2 framing 校验；strata 的完整性校验在 blob 内容层（见 exact_reuse_trust 的改写行） |
| layerfs-storage/tests/verified_pack_read.rs::c2_still_refuses_hash_valid_pack_with_wrong_domain | C2 仍然拒绝哈希合法但域错误的 pack | — | 不保留 | 同上 |
| layerfs-persistence/tests/acquisition_batching.rs::non_host_windows_skip_the_host_insert_without_losing_targets_or_charges | 非 host 窗口跳过 host 插入，且不丢目标与计费 | — | 不保留 | 实现细节（acquisition profile）：acquisition 的 SQL 窗口执行成本、别名身份与原子根完成；strata 没有 acquisition 表 |
| layerfs-persistence/tests/acquisition_batching.rs::narrow_bindings_preserve_mixed_payloads_after_an_existing_alias_boundary | 窄绑定在已有别名边界之后仍保持混合 payload | — | 不保留 | 同上 |
| layerfs-persistence/tests/acquisition_batching.rs::directory_uniqueness_and_host_evidence_keep_the_first_refusal_after_a_prefix | 目录唯一性与 host 证据在前缀之后仍保留第一个拒绝 | — | 不保留 | 同上 |
| layerfs-persistence/tests/acquisition_batching.rs::windows_amortize_execution_without_losing_aliases_or_atomic_completion | 窗口摊薄执行成本，同时不丢别名与原子完成 | — | 不保留 | 同上 |
| layerfs-persistence/tests/acquisition_batching.rs::missing_and_repeated_roots_roll_back_the_window_and_report_first_failure | 缺失与重复的根使整个窗口回滚，并报告第一个失败 | — | 不保留 | 同上 |
| layerfs-persistence/tests/acquisition_batching.rs::directory_completion_uses_only_directory_positions_and_is_atomic | 目录完成只使用目录位置并且是原子的 | — | 不保留 | 同上 |
| layerfs-persistence/tests/acquisition_batching.rs::root_window_index_work_ignores_unrelated_population | 根窗口的索引工作量不受无关数据量影响 | — | 不保留 | 同上 |
| layerfs-persistence/tests/acquisition_batching.rs::independent_entries_share_executions_and_population_work_stays_indexed | 相互独立的条目共享执行，数据量增长时工作量仍走索引 | — | 不保留 | 同上 |
| layerfs-persistence/tests/acquisition_batching.rs::entry_refusal_order_and_full_unit_rollback_survive_execution_windows | 条目拒绝顺序与整单元回滚在跨执行窗口时保持 | — | 不保留 | 同上 |
| layerfs-persistence/tests/acquisition_batching.rs::a_metadata_value_outside_the_portable_grammar_is_refused_and_never_read | 不符合 portable 语法的元数据值（如 mode 不是 0o777 的符号链接）在写入时被拒，读解码也拒绝 | — | 不保留 | 实现细节（文件系统树编码）：portable 元数据语法；strata 的 overlay 只记 mode |
| layerfs-persistence/tests/acquisition_disposal.rs::final_bounded_disposal_releases_in_one_ack_and_preserves_another_owner | 有界的最终清理在一次确认里释放，并保留另一个 owner 的工作 | — | 不保留 | 实现细节（acquisition profile）：acquisition 的 SQL 窗口执行成本、别名身份与原子根完成；strata 没有 acquisition 表 |
| layerfs-persistence/tests/acquisition_disposal.rs::zero_budget_retains_work_and_can_release_an_empty_owner | 零预算时保留工作，仍可释放空 owner | — | 不保留 | 同上 |
| layerfs-persistence/tests/acquisition_reads.rs::valid_empty_missing_released_and_foreign_owners_remain_distinct | 有效的空、缺失、已释放与外来 owner 彼此可区分；确定的输入拒绝不使 session 失效 | — | 不保留 | 实现细节（acquisition profile）：acquisition 的 SQL 窗口执行成本、别名身份与原子根完成；strata 没有 acquisition 表 |
| layerfs-persistence/tests/acquisition_reads.rs::typed_reads_and_narrow_bindings_preserve_operation_isolation_order_and_bounds | 带类型的读取与窄绑定保持操作隔离、顺序与上界 | — | 不保留 | 同上 |
| layerfs-persistence/tests/concurrent_first_installs.rs::two_writers_saving_the_same_new_content_both_complete | 两个写入者各持一个打开的 Store handle，同时保存同一份新内容：两边都成功，每个对象都能读回，谁赢得 first-wins 注册都一样 | store | 改写 | 改写为 store::put_blob 的并发幂等用例：两个会话同时 checkpoint 同一份新内容，两边都成功、只落一个 blob、读回完整 |
| layerfs-persistence/tests/concurrent_pooled_saves.rs::earlier_reserved_save_finishes_after_a_higher_block_was_published | 先预留的 Save 在后预留的更高块已发布之后才结束，预留顺序与发布顺序无关 | — | 不保留 | 实现细节（inode 值池化）：池化值序号块的预留顺序；strata 无池化值 |
| layerfs-persistence/tests/diagnostics_during_lane.rs::counters_are_read_while_the_lane_is_held | 读取 session 的 SQL 计数器不等待持有 publication lane 的 Commit | — | 不保留 | 实现细节（SQL 计数器诊断）：session 计数器的读取不排队（F-018）；strata 无此计数器 |
| layerfs-persistence/tests/disposable_startup_sync.rs::disposable_create_save_reopen_seal_never_calls_vfs_sync | Disposable 的创建、保存、重开与封存全程从不调用 VFS sync | — | 不保留 | 实现细节（Disposable profile）：synchronous=OFF 且不 fsync 的 Disposable profile；strata 只用 WAL + synchronous=FULL（设计 §11.8） |
| layerfs-persistence/tests/file_lengths.rs::a_cold_whole_file_length_uses_only_indexed_catalogue_rows | 冷启动时整文件长度只用带索引的 catalogue 行，不读 pack、不读 BLOB | — | 不保留 | 实现细节（acquisition profile）：只用目录行（catalogue）取文件长度而不物化 payload；strata 的文件长度直接来自 stat |
| layerfs-persistence/tests/file_lengths.rs::mixed_lengths_preserve_order_without_fetching_chunk_payloads | 混合长度保持请求顺序，且不取 chunk payload；缺失的对象 id 报 ObjectMissing | — | 不保留 | 同上 |
| layerfs-persistence/tests/history_allocation.rs::reservations_are_monotone_half_open_and_per_scope | scope 内的预留单调、左闭右开且按 scope 独立：不同 scope 各自从自己的起点开始，不共用区间 | store | 改写 | 改写为 store::record_checkpoint 的 id 分配用例：CheckpointId 按会话单调递增、互不共用（批量半开区间不保留，strata 逐个分配） |
| layerfs-persistence/tests/history_allocation.rs::a_consumed_reservation_survives_a_later_failure | 预留的区间在发出时就已消耗：之后创建成功或失败，计数都不回退 | store | 改写 | 改写为 store 的 id 不回收用例：已分配的 CheckpointId 在后续失败后不复用，失败留下空洞，下一个 id 继续递增 |
| layerfs-persistence/tests/history_allocation.rs::counts_are_checked_and_the_terminal_endpoint_is_refused | 数量受检查，终点（i64::MAX）被拒绝而不是溢出：越过终点的区间不可表示也不可报告 | store | 改写 | 改写为 store 的 id 终点用例：CheckpointId 是 u64，但落进 SQLite INTEGER（i64），分配到 i64::MAX 之后必须拒绝而不是回绕 |
| layerfs-persistence/tests/history_remediation.rs::all_legal_name_widths_paginate_by_identity | 所有合法的名字宽度都能按身份翻页 | — | 不保留 | 实现细节（History 目录）：History 目录（Branch/Commit/Layer 元数据、翻页游标与锚点）的回归；strata 的会话、checkpoint 与发布记录是自己的表，不用游标翻页 |
| layerfs-persistence/tests/history_remediation.rs::advancing_the_actual_subject_preserves_both_anchor_forms | 推进实际的主体时，两种锚点形式都保持 | — | 不保留 | 同上 |
| layerfs-persistence/tests/history_remediation.rs::forged_sibling_position_and_obsolete_checksum_are_refused | 伪造的兄弟位置与过时的校验和被拒 | — | 不保留 | 同上 |
| layerfs-persistence/tests/history_remediation.rs::read_only_cursor_capability_survives_reopen_and_rejects_wrong_keys | 只读游标能力在重开后仍然有效，错误的密钥被拒 | — | 不保留 | 同上 |
| layerfs-persistence/tests/history_remediation.rs::empty_history_checks_page_and_cursor_before_returning_empty | 空历史也先检查页与游标，再返回空 | — | 不保留 | 同上 |
| layerfs-persistence/tests/history_remediation.rs::last_representable_reservation_is_consumed_and_terminal_refusal_is_atomic | 最后一个可表示的预留被消耗，终点之后的拒绝是原子的 | store | 改写 | 改写为 store 的 id 终点用例（同 history_allocation::counts_are_checked…）：到达 i64::MAX 后拒绝，且拒绝不改变已存状态 |
| layerfs-persistence/tests/history_remediation.rs::reopen_refuses_each_independent_schema_or_binding_corruption | 重开时对每一种独立的 schema 或绑定篡改都拒绝 | store | 改写 | 改写为 store::open 的 schema 校验用例：缺表、缺约束或列类型被改的 Store 都被拒绝打开，不尝试修补；binding key 一项不保留 |
| layerfs-persistence/tests/history_remediation.rs::exact_source_publication_revalidates_immutable_provenance | 按原样来源重新发布时，重新校验已存 Layer 的不可变来源（被篡改则报 Integrity） | — | 不保留 | 实现细节（History 目录）：已发布 Layer 的 provenance 复核；strata 发布链以 git ref 比较并交换为准，不复核 Store 行 |
| layerfs-persistence/tests/immutable_reads_under_reclamation.rs::immutable_reads_are_unchanged_while_free_pages_are_reclaimed | 另一个会话回收空闲页（会移动文件内的页）期间，只读会话的 locate 与对象读取结果不变 | — | 不保留 | 实现细节（空间回收）：SQLite 空闲页回收（auto_vacuum）移动文件内页；strata 的 blob 是独立文件，gc 删除未引用 blob 不会移动其他 blob |
| layerfs-persistence/tests/inode_branch_delta_custody.rs::a_branch_base_cannot_be_dropped_and_survives_reclamation | delta 存储的 branch 所指名的 base，只要该 branch 还在就一直可读：已发布的 pack 与 pack 单元不能被删除或改动（schema 中止该语句，报 immutable），空间回收只释放未用页，所有表读回一致 | store | 改写 | 改写为 store 的不可变与引用保护用例：被引用的 blob 与已提交的 checkpoint 行不可被 UPDATE/DELETE（schema 层拒绝，报 immutable），gc 只删无引用且过保留期者（gc_keeps_referenced_generations）；delta base 一项不保留 |
| layerfs-persistence/tests/inode_branch_delta_custody.rs::two_saves_choosing_the_same_published_base_both_store_deltas_and_read_back | 两个 Save 同时选中同一个已发布的 base，都存为 delta branch 且都能读回；完全相同的改动则命中已存对象 | — | 不保留 | 实现细节（delta 编码）：并发 Save 对同一 delta base 的选择；strata 无 delta |
| layerfs-persistence/tests/interleaved_saves.rs::scoped_registry_interleaves_pending_reads_finish_abort_and_slot_reuse | 多个 Save 交错：B 结束时 A、C 仍在，各自的待读互不影响；被放弃的 Save 的对象读不到；槽位可复用 | store | 改写 | 改写为 store 的多会话交错用例：一个会话的 checkpoint 完成后其他会话仍在进行，已完成者可读，被放弃（失败）者不可见，互不影响；scoped registry 与槽位复用不保留 |
| layerfs-persistence/tests/lineage_backfill.rs::a_pre_lineage_store_is_backfilled_at_open_and_answers_the_same | 早于 lineage 表的 Store 可写打开后获得新表并回填：祖先、merge base 与覆盖计数与之前相同，发布继续 | — | 不保留 | 实现细节（Store 迁移）：lineage 表（#353）的增量迁移与回填；strata v1 没有历史 schema 要迁移 |
| layerfs-persistence/tests/lineage_backfill_measure.rs::backfill_on_a_store_copy | 在真实 Store 副本上手工度量一次回填 lineage 表的开销（#[ignore]，需环境变量） | — | 不保留 | 实现细节（Store 迁移）：lineage 表的回填与度量（#353）；strata v1 没有历史 schema 要迁移 |
| layerfs-persistence/tests/merge_records.rs::records_are_private_guarded_and_removed | 每个 merge 只看到自己的记录，带守卫的批要么整体应用要么不应用，end 删除记录，超过常驻上限后写入文件 | — | 不保留 | 实现细节（merge 构建记录）：把 merge 构建过程的记录存进 Store 文件（私有、带守卫的批、空闲 owner 租约清扫、常驻上限）；strata 的三方合并在 staging 目录里由 gitio::merge3 完成，不落库 |
| layerfs-persistence/tests/merge_records.rs::open_sweeps_only_idle_owners | 可写打开只清扫空闲超过租约的 owner；被清扫的 owner 之后的调用失败而不是把记录读成不存在 | — | 不保留 | 同上 |
| layerfs-persistence/tests/merge_records.rs::key_windows_read_the_primary_key_without_a_scan | key 窗口按主键范围读取，没有全表扫描与排序（#364） | — | 不保留 | 同上 |
| layerfs-persistence/tests/merge_records.rs::a_merge_past_the_resident_bound_answers_as_a_resident_one | 超过常驻上限的 merge 与常驻的 merge 给出相同答案 | — | 不保留 | 同上 |
| layerfs-persistence/tests/old_format_open.rs::an_old_format_store_opens_and_records_hints_from_then_on | 早于 binding_hint 表的 Store 可写打开后获得该表，已有内容不被改写，此后开始记录 hint；只读权限不能加表，仍被拒 | — | 不保留 | 实现细节（Store 迁移）：binding_hint 表的增量迁移（#352 F-011）；strata v1 没有旧版 Store，schema 版本策略由 store 文档规定 |
| layerfs-persistence/tests/old_format_open.rs::a_store_with_any_other_schema_difference_is_still_refused | 与预期有任何其他 schema 差异的 Store 仍被拒绝，不尝试修补 | store | 改写 | 改写为 store::open 的 schema 校验用例：user_version 或表结构与预期不符的 Store 被拒绝打开，不迁移、不修补 |
| layerfs-persistence/tests/seal_allocation.rs::seal_releases_excess_and_preserves_original_file_for_all_layouts | 封存释放多余分配并保留原文件字节与同目录的无关文件，各种布局都一样 | — | 不保留 | 实现细节（Store 封存）：封存 Store 文件（释放超出逻辑 EOF 的 extent、保留原文件字节；忙/只读/硬链接/符号链接时拒绝）；strata 不需要把 Store 封成可分发的单文件 |
| layerfs-persistence/tests/seal_allocation.rs::shared_session_busy_leaves_allocation_untouched | 共享 session 忙时封存报 Busy，分配不变 | — | 不保留 | 同上 |
| layerfs-persistence/tests/seal_allocation.rs::readonly_seal_does_not_release_extents | 只读封存不释放 extent | — | 不保留 | 同上 |
| layerfs-persistence/tests/seal_allocation.rs::hard_link_refusal_preserves_original_bytes_and_allocation | 路径是硬链接时拒绝封存，原字节与分配都保持 | — | 不保留 | 同上 |
| layerfs-persistence/tests/seal_allocation.rs::symlink_refusal_preserves_original_bytes_and_allocation | 路径是符号链接时拒绝封存，原字节与分配都保持 | — | 不保留 | 同上 |
| layerfs-persistence/tests/semantic_admission.rs::admitted_complete_root_closes_and_reconstructs_through_the_real_store | 已准入的完整根在真实 Store 中闭合并能重建；生产者在 finish 之前就能看到已准入的对象 | — | 不保留 | 实现细节（对象信封）：运行时对象的准入与依赖闭包；strata 的 blob 之间没有父子依赖 |
| layerfs-persistence/tests/semantic_admission.rs::omitted_wire_references_cannot_hide_an_inode_dependency_from_save | 线上传来的对象即使不声明引用，运行时也要自行推导；Save 不能被藏起来的依赖骗过，finish 时缺失依赖报 MissingDependency | store | 改写 | 改写为 store 的引用完整性用例：checkpoint 引用了 Store 里不存在的 blob 时，提交被拒绝，不留悬空引用（引用由 store 依据 overlay 条目推导，不信任调用方自报） |
| layerfs-persistence/tests/serverless_store.rs::lock_child | 另一个进程持有未提交写事务的子进程半部（由父测试以环境触发，其 socket 超时会在父进程消失时结束原事务） | store | 改写 | 改写为'外部写者占锁'用例的子进程半部（见 second_process_busy_has_no_effect_and_readers_keep_progressing） |
| layerfs-persistence/tests/serverless_store.rs::disposable_create_publish_seal_and_reopen_all_layouts | Disposable Store 的创建、发布、封存与重开，覆盖所有 pack 布局，封存后目录里只剩一个文件 | — | 不保留 | 实现细节（Disposable profile）：synchronous=OFF 的 Disposable 无服务 Store 及其封存；strata 只用 WAL + FULL，不封存 |
| layerfs-persistence/tests/serverless_store.rs::second_process_busy_has_no_effect_and_readers_keep_progressing | 另一个进程持有未提交的写事务时：本进程的写入立即报 Busy 且没有副作用；新开的独立读连接照常完成，读者不被阻塞；之后的显式调用能继续推进 | store | 改写 | 改写为 store 的外部写者用例：外部进程占着 SQLite 写锁时本进程写入立即失败、不重试、无副作用（错误码归类同上一条）；WAL 下 CLI 只读连接不被写者阻塞（对应'daemon 在写、CLI 只读'） |
| layerfs-persistence/tests/serverless_store.rs::seal_refuses_retained_provider_without_effect | 仍持有 provider 时封存报 Busy 且无副作用 | — | 不保留 | 实现细节（Store 封存）：封存 Store 文件；strata 不封存 |
| layerfs-persistence/tests/serverless_store.rs::non_wal_store_is_refused_without_conversion | journal_mode 不是 WAL 的 Store 被拒绝，不做转换 | store | 改写 | 改写为 store::open 的 journal 校验用例：不是 WAL 的数据库被拒绝，不转换其 journal 模式 |
| layerfs-persistence/tests/signature_ring_order.rs::a_wrapped_ring_reads_back_in_stamp_order_and_proposes_its_newest_entries | delta 候选签名环按槽位序读出后按 stamp 排序；写满回绕后顺序与按 stamp 排序读取一致，全新的 Storage 能找到回绕后写入的条目作 delta base | — | 不保留 | 实现细节（delta 编码）：delta 候选签名环（content_signature）；strata 不选 delta 候选 |
| layerfs-persistence/tests/space_reclamation.rs::bounded_reclamation_preserves_another_live_operation_and_survives_reopen | 有界的空间回收不影响另一个进行中的操作，且重开后保持（auto_vacuum=2，每次最多回收 RECLAMATION_PAGE_LIMIT 页） | — | 不保留 | 实现细节（空间回收）：SQLite 增量回收的页数上限；strata 的 gc 按引用计数与保留期删 blob，不做页级回收 |
| layerfs-persistence/tests/space_reclamation.rs::old_non_reclaimable_stores_are_opened_without_implicit_conversion | 旧的、不可回收的 Store 不做隐式转换地打开，请求回收报 Malformed | — | 不保留 | 同上 |
| layerfs-persistence/tests/space_reclamation.rs::path_windows_use_actual_lengths_without_exceeding_column_or_row_budgets | 路径窗口按实际长度取值，不超过列预算与行预算（512 行、256 KiB/(4096+68)） | — | 不保留 | 实现细节（acquisition profile）：acquisition 路径窗口的列/行预算 |
| layerfs-persistence/tests/sqlite_group_rows.rs::explicit_creation_reopens_declared_units_and_keeps_sparse_canonical_order | 显式创建的声明单元在重开后保持，稀疏的 canonical 顺序保持 | — | 不保留 | 实现细节（SQLite 包布局）：schema2 的 group/unit 行、完整 pack 重组与 locator 预算；strata 的 blob 是普通文件 |
| layerfs-persistence/tests/sqlite_group_rows.rs::complete_reads_reassemble_exact_bytes_and_digest_for_every_lane | 完整读取对每条 lane 都能重组出精确的字节与摘要；不可变读取不开 BEGIN/COMMIT | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_group_rows.rs::unread_corruption_is_scoped_and_whole_audit_refuses_it | 未读到的损坏只影响整体审计，不影响范围读取 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_group_rows.rs::malformed_unit_extent_is_refused_before_any_unit_blob_read | 畸形的单元 extent 在读任何单元 BLOB 之前被拒 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_group_rows.rs::late_locator_failure_rolls_back_control_and_every_unit | 较晚的 locator 失败使控制行与所有单元一并回滚 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_group_rows.rs::exact_physical_fanout_refuses_over_budget_batch_before_begin | 物理扇出超预算的批在 BEGIN 之前就被拒（16 个 group = 1 个 pack 行 + 16 个单元行 + 8,180 个 locator，超出 6 行） | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_group_rows.rs::oversized_control_is_rejected_before_group_blob_acquisition | 过大的控制区在取 group BLOB 之前被拒 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_group_rows_indexed.rs::declared_layouts_reopen_without_mutating_or_promoting_their_schema | 已声明的布局重开时不改动、也不提升其 schema | — | 不保留 | 实现细节（SQLite 包布局）：schema3 的覆盖索引、legacy 重开与 typed 读取游标；strata 的 blob 是普通文件 |
| layerfs-persistence/tests/sqlite_group_rows_indexed.rs::mapping_is_covered_and_complete_pack_reassembles_exactly | 映射由覆盖索引覆盖，完整 pack 精确重组 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_group_rows_indexed.rs::missing_covering_index_is_refused_at_open_without_automatic_repair | 缺失覆盖索引的 Store 在打开时被拒，不自动修复 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_group_rows_indexed.rs::covered_mapping_still_refuses_wrong_extents_before_blob_open | 覆盖映射仍在打开 BLOB 之前拒绝错误的 extent | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_group_rows_indexed.rs::multigroup_read_reopens_one_cursor_and_never_retains_it_between_requests | 多 group 读取重开一个游标，请求之间不保留它 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_group_rows_indexed.rs::wrong_reopened_body_length_is_refused_and_cursor_is_checked_closed | 重开的 body 长度不符被拒，游标被检查为已关闭 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_group_rows_indexed.rs::typed_singleton_matches_batch_results_missing_ids_and_statement_boundary | 带类型的单点查询与批量结果一致，缺失 id 与语句边界也一致（一次 SELECT、无 BEGIN/COMMIT） | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_group_rows_indexed.rs::typed_descriptor_rejects_invalid_domain_digest_and_length_before_blob_open | 带类型的描述符在打开 BLOB 之前拒绝非法 domain、摘要与长度 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_group_rows_indexed.rs::typed_singleton_rejects_invalid_role_and_signed_coordinates | 带类型的单点查询拒绝非法 role 与有符号坐标 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_inode_values.rs::values_come_back_in_ordinal_order_from_one_statement_of_one_transaction | 值按序号顺序用一条语句、一个事务读出：成段、间隙与零散行都对，没有行的序号被省略；不可变值读取不 BEGIN/COMMIT | — | 不保留 | 实现细节（inode 值池化）：inode 值作为按序号存储的行（一次语句读取、从不替换）；strata 无 inode 值行 |
| layerfs-persistence/tests/sqlite_inode_values.rs::a_malformed_set_is_refused_before_any_statement | 畸形的值集合在任何语句之前被拒 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_inode_values.rs::a_stored_value_is_never_replaced_and_a_refused_batch_leaves_no_row | 已存的值永不被替换；批里只要有一个序号已有值，整批被拒（包括它前后的行），不留任何行 | store | 改写 | 改写为 store 的 append-only 用例：已存在的行（blob、overlay 条目、checkpoint）不被替换，冲突的批整体被拒、不留半个批 |
| layerfs-persistence/tests/sqlite_inode_values.rs::a_row_changed_in_the_file_fails_the_read_of_its_leaf | 文件里某一行被改动或删除，其叶页读取失败（长度不符的值被表本身拒绝） | — | 不保留 | 实现细节（inode 值池化）：inode 值作为按序号存储的行（一次语句读取、从不替换）；strata 无 inode 值行 |
| layerfs-persistence/tests/sqlite_inode_values.rs::a_store_of_the_value_group_format_is_refused_at_open | 旧格式的 Store（版本 1–6 用 metadata_value_group，7 与 13 早于 commit-v3）在打开时被拒，不迁移 | store | 改写 | 改写为 store::open 的版本拒绝用例：user_version 属于旧格式或未知版本的 Store 在打开时被拒，不迁移 |
| layerfs-persistence/tests/sqlite_lock_custody.rs::cooperative_release_preserves_sqlite_locks_with_two_live_sessions | 两个存活 session 时，协作式释放保持 SQLite 自己的 POSIX 锁 | — | 不保留 | 实现细节（SQLite 锁托管）：LayerFS 自己的 session 协作式就绪协议对 SQLite POSIX 锁的托管（同进程两个 live session 互不丢锁）；strata 直接用一个写连接，锁由 SQLite 管理 |
| layerfs-persistence/tests/sqlite_lock_custody.rs::dropping_one_session_preserves_the_other_sessions_sqlite_locks | 丢弃其中一个 session 不丢另一个 session 的 SQLite 锁 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_ordinal_lookahead.rs::fresh_leaves_share_one_combined_reservation_and_deduplicate_values | 新叶页共用一次合并预留并对相同的值去重 | — | 不保留 | 实现细节（inode 值池化）：合并 Save 的序号预留与前瞻、已确认的空洞；strata 无池化序号 |
| layerfs-persistence/tests/sqlite_ordinal_lookahead.rs::a_foreign_acknowledged_reservation_is_never_recycled_by_the_plan | 别人已确认的预留不会被本计划回收 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_ordinal_lookahead.rs::small_configured_blocks_refill_without_limiting_the_save | 配置得很小的块会续领，而不限制 Save | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_ordinal_lookahead.rs::initial_combined_reservation_is_busy_before_effect | 初始的合并预留在产生任何效果之前就报 Busy | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_ordinal_lookahead.rs::content_reads_retain_the_original_same_save_storage_failure | 内容读取保留同一 Save 原始的存储失败 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_pack_selection.rs::sqlite_selected_ranges_scan_once_close_and_match_the_full_body | 真实 SQLite 的偏移读取：选定范围只扫描一次、关闭，并与完整 body 的摘要和字节一致；检查通过的关闭/提交后 session 仍可用 | — | 不保留 | 实现细节（SQLite 包布局）：pack BLOB 的偏移读取与整体摘要；strata 的 blob 是普通文件 |
| layerfs-persistence/tests/sqlite_publication.rs::wal_full_settings_and_unavailable_engine_are_explicit | WAL + synchronous=FULL（及 fullfsync 等）设置被显式请求并回读核对；引擎不可用时显式拒绝；Store 约束与表集合固定 | store | 改写 | 改写为 store::open 的持久配置用例（checkpoint_write_is_durable_after_crash 的前置）：打开后 PRAGMA journal_mode=wal、synchronous=FULL（FULL=2）回读一致，设置不符则拒绝；表名数量与平台降级不保留 |
| layerfs-persistence/tests/sqlite_publication.rs::final_body_multirow_locators_and_first_wins_have_one_atomic_ack | 最终 body、多行 locator 与 first-wins 注册在一个事务里、一次 COMMIT 确认（一个 body INSERT、集合式 locator、一次总计 UPDATE） | store | 改写 | 改写为 store::record_checkpoint 的原子单元用例：checkpoint 行、overlay 条目与 blob 登记在一个事务里一次提交；已存在的 blob 登记按 first-wins 忽略；语句条数断言不保留 |
| layerfs-persistence/tests/sqlite_publication.rs::failure_after_body_insert_rolls_back_the_entire_unit_and_published_body_is_immutable | body 插入之后失败，整个单元回滚；已发布的 body 不可变（直接 UPDATE/DELETE 被 schema 中止） | store | 改写 | 改写为 store 的回滚与不可变用例：record_checkpoint 中途失败时整体回滚，不留半个 checkpoint；已提交的 checkpoint/blob 行不可被 UPDATE 或 DELETE |
| layerfs-persistence/tests/sqlite_publication.rs::readonly_refusal_and_external_writer_busy_do_not_retry | 只读 handle 拒绝写；外部写者占锁时本进程写入立即 Busy，不重试 | store | 改写 | 改写为 store 的只读与忙用例：只读打开拒绝一切写（Code::ReadOnly；daemon 不在时 CLI 只读，设计 §5）；SQLite 写锁被占时写事务立即失败、不重试、不产生语句副作用（该失败的错误码归类由 store 文档规定，Code::Busy 在 types 里专指 daemon 的 repo 写锁） |
| layerfs-persistence/tests/sqlite_publication.rs::canonical_save_read_reuse_and_persisted_reservations_survive_reopen | 保存、读取、复用，以及持久化的预留，在重开之后都还在 | store | 改写 | 改写为 store 的重开用例：写入的 blob 与 checkpoint 重开后仍可读；相同内容再次写入计为复用；已分配的 CheckpointId 重开后继续递增 |
| layerfs-persistence/tests/sqlite_publication.rs::writable_open_does_not_migrate_an_unrelated_database_journal | 可写打开不会把无关数据库的 journal 改成 WAL：对不是 Store 的数据库打开失败，其 journal 仍是 delete，也不产生 -wal 文件 | store | 改写 | 改写为 store::open 的无关数据库用例：对不是 Store 的 SQLite 文件拒绝打开，且不改动它（不改 journal 模式、不留 -wal/-shm） |
| layerfs-persistence/tests/sqlite_publication.rs::signature_publication_is_bounded_set_based_and_preserves_stamps | 签名行在一个原子单元里集合式、分页地发布，并保持 stamp | — | 不保留 | 实现细节（delta 编码）：delta 候选签名行（content_signature）的分页发布 |
| layerfs-persistence/tests/sqlite_publication.rs::failing_signature_page_rolls_back_prior_body_and_all_signature_rows | 较晚的签名页失败时，先前的 body 与所有签名行一并回滚 | — | 不保留 | 实现细节（delta 编码）：签名页失败的回滚；整单元回滚的通则见 failure_after_body_insert… 的改写行 |
| layerfs-persistence/tests/sqlite_publication.rs::signature_pages_share_one_acknowledgement_at_the_existing_row_bound | 签名页在既有行数上界内共用一次确认 | — | 不保留 | 实现细节（delta 编码）：签名页的确认粒度 |
| layerfs-persistence/tests/sqlite_publication.rs::pack_insert_pages_preserve_bodies_and_atomic_conflict_refusal | pack 分页 INSERT（越过 512 行语句上界）保持 body，冲突整体被拒；新首页在第二页冲突时也回滚 | — | 不保留 | 实现细节（SQLite 包布局）：pack/签名行的分页 INSERT、语句数与 BLOB 绑定所有权；strata 的 blob 是普通文件，没有分页发布 |
| layerfs-persistence/tests/sqlite_publication.rs::pack_insert_pages_keep_large_blob_binding_ownership_separate | 分页 INSERT 时大 BLOB 的绑定所有权互相独立，总计只更新一次 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_publication.rs::explicit_profiles_keep_atomic_publication_and_matching_reopen_contracts | Durable 与 Disposable 两种 profile 都保持原子发布，并以匹配的 profile 重开；只读重开报告同一 profile，且不能封存 | — | 不保留 | 实现细节（Disposable profile）：Durable/Disposable 双 profile 的并存与重开；strata 只有 Durable（WAL + FULL） |
| layerfs-persistence/tests/sqlite_publication.rs::multi_page_publication_preserves_first_wins_loss_order_and_late_failure_atomicity | 多页发布保持 first-wins 的落败顺序，且较晚失败时仍然原子 | — | 不保留 | 实现细节（SQLite 包布局）：pack/签名行的分页 INSERT、语句数与 BLOB 绑定所有权；strata 的 blob 是普通文件，没有分页发布 |
| layerfs-persistence/tests/sqlite_recovery.rs::crash_child | SIGKILL 恢复用例的子进程半部：确认一个单元后等待被杀（环境变量未设置时直接返回） | store | 改写 | 改写为 checkpoint_write_is_durable_after_crash 的子进程半部：record_checkpoint 确认后，等待父进程 SIGKILL |
| layerfs-persistence/tests/sqlite_recovery.rs::sigkill_preserves_acknowledged_body_locator_and_reservation | 进程在确认之后被 SIGKILL：重开后已确认的 body、locator 与预留都在，下一个预留紧接其后 | store | 改写 | 改写为 store 的 checkpoint_write_is_durable_after_crash：确认的 checkpoint 行、blob 与已分配的 CheckpointId 在 SIGKILL 后重开仍在，下一个 id 紧接其后（synchronous=FULL 的验证方法；不声称断电级持久） |
| layerfs-persistence/tests/sqlite_reference_acquisition.rs::new_extent_leaf_checks_child_membership_without_reading_child_packs | 新 extent 叶页检查子对象成员关系时不读子对象的 pack；叶页与每个被引用的 payload 仍可认证、可读 | — | 不保留 | 实现细节（acquisition profile）：成员检查走索引而不重建子 payload |
| layerfs-persistence/tests/sqlite_reference_acquisition.rs::missing_logical_child_is_still_refused | 缺失的逻辑子对象仍然被拒绝 | store | 改写 | 改写为 store 的引用完整性用例：checkpoint 引用了不存在的 blob 时拒绝提交（与 semantic_admission::omitted_wire_references… 同一性质） |
| layerfs-persistence/tests/sqlite_transaction_units.rs::canonical_and_shared_physical_bytes_are_bounded_independently | canonical 字节与共享物理字节各自独立地受 4 MiB 上界约束 | — | 不保留 | 实现细节（SQLite 包布局）：写事务单元的字节记账、预留尾部与 pack 队列；strata 的 blob 是普通文件 |
| layerfs-persistence/tests/sqlite_transaction_units.rs::each_save_owns_a_new_combined_range_without_recycling_used_ids | 每个 Save 拥有新的合并区间，不回收已用的 id | — | 不保留 | 实现细节（SQLite 包布局）：pack id 与序号的合并区间预留；CheckpointId 的不回收性质见 history_allocation 的改写行 |
| layerfs-persistence/tests/sqlite_transaction_units.rs::dropping_unfinished_save_does_not_return_consumed_ids_to_the_unused_tail | 丢弃未完成的 Save 不把已消耗的 id 还给未用尾部 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_transaction_units.rs::reader_reuses_authenticated_pack_within_one_operation | 同一次操作内读者复用已认证的 pack；新操作的缓存为空，要重新付出获取代价 | — | 不保留 | 实现细节（acquisition profile）：pack 在一次操作内的复用 |
| layerfs-persistence/tests/sqlite_transaction_units.rs::partial_pack_queue_survives_wave_boundary_within_existing_budget | 未装满的 pack 队列跨越 wave 边界仍保留，且在既有预算内 | — | 不保留 | 实现细节（SQLite 包布局）：写事务单元的字节记账、预留尾部与 pack 队列；strata 的 blob 是普通文件 |
| layerfs-persistence/tests/sqlite_transaction_units.rs::same_save_read_and_dependency_force_closure_of_carried_queue | 同一 Save 内的读取与依赖检查强制闭合已携带的队列 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_transaction_units.rs::a_small_acknowledged_pack_tail_serves_a_larger_following_wave | 较小的已确认 pack 尾部可供更大的后续 wave 使用 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_transaction_units.rs::allocation_replenishes_before_pressure_seals_when_the_tail_is_consumed | 尾部耗尽时先补充分配，再因压力封 pack | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_transaction_units.rs::successful_save_history_retains_selection_and_group_work_from_final_drain | 成功的 Save 历史保留最终排空阶段的选择与 group 工作量 | — | 不保留 | 同上 |
| layerfs-persistence/tests/sqlite_transaction_units.rs::queued_chunk_groups_use_the_acknowledged_pack_tail | 排队的 chunk group 使用已确认的 pack 尾部；每个 Save 各有自己的块，排队 group 仍共用完整 pack | — | 不保留 | 同上 |
| layerfs-persistence/tests/thread_work.rs::each_thread_counts_only_its_own_sql_work | 线程的 SQL 总计只含它自己执行的工作；共享一个 session 的线程分摊 session 的计数器 | — | 不保留 | 实现细节（SQL 计数器诊断）：按线程拆分 session 的 SQL 工作计数（#367）；strata 无此计数器 |
| layerfs-persistence/tests/writer_admission.rs::shared_session_reservation_waits_for_an_ordinary_public_read_owner | 共享 session 的预留等待一个普通的公共读 owner | — | 不保留 | 实现细节（协作式写者准入）：LayerFS 在 SQLite BEGIN 之前自己做的准入（共享 session、跨进程 flock、内核等待者、可重入检测）；strata 的写者只有 daemon 内的单写连接，其排队性质见 daemon 的改写行 |
| layerfs-persistence/tests/writer_admission.rs::another_process_admission_precedes_the_single_sqlite_begin | 另一个进程的准入先于唯一一次 SQLite BEGIN（仅 Linux 编译） | — | 不保留 | 同上 |
| layerfs-persistence/tests/writer_admission.rs::two_opened_handles_admit_eight_distinct_reservations_without_conflict | 同一进程里两个已打开的 handle 准入八个互不冲突的预留，busy_timeout 仍为 0（仅 Linux 编译） | — | 不保留 | 实现细节（SQLite 包布局）：pack id 与序号预留的并发分配 |
| layerfs-persistence/tests/writer_admission.rs::queued_writers_outlive_the_old_window_and_all_succeed | 排队的写者等待超过旧的等待窗口后仍全部成功，没有人因超时失败 | daemon | 改写 | 改写为 daemon 的写锁排队用例：同一 repo 的并发写请求排队，持有者在等待上限内结束时全部成功；超过等待上限才报 Code::Busy（types：'在等待上限内没拿到'）；LayerFS 取消固定窗口后的无上限等待不保留（设计 §8：daemon 内按 repo 一把写锁） |
| layerfs-persistence/tests/writer_admission.rs::a_cross_process_waiter_outlives_the_old_window_and_succeeds | 跨进程的等待者超过旧窗口后仍然成功（仅 Linux 编译） | — | 不保留 | 实现细节（协作式写者准入）：LayerFS 在 SQLite BEGIN 之前自己做的准入（共享 session、跨进程 flock、内核等待者、可重入检测）；strata 的写者只有 daemon 内的单写连接，其排队性质见 daemon 的改写行 |
| layerfs-persistence/tests/writer_admission.rs::queued_writers_are_admitted_first_come_first_served | 排队的写者按先到先得的顺序被准入 | daemon | 改写 | 改写为 daemon 的写锁公平性用例：同一 repo 的写请求按到达顺序依次执行 |
| layerfs-persistence/tests/writer_admission.rs::closing_the_store_fails_every_queued_writer_typed_and_lets_the_holder_finish | 关闭 Store 时每个排队的写者都以带类型的错误（Closed）失败，持有者先完成；之后的写入同样被拒 | daemon | 改写 | 改写为 daemon 的停机用例：停止 daemon 时排队中的写请求以明确的错误失败，正在执行的请求先完成，停机之后的新写入被拒 |
| layerfs-persistence/tests/writer_admission.rs::writers_queue_behind_a_held_publication_lane_without_deadlock | 写者排在被持有的 publication lane 之后而不死锁；持有者自己的写在其持有的回合内执行 | — | 不保留 | 实现细节（协作式写者准入）：LayerFS 在 SQLite BEGIN 之前自己做的准入（共享 session、跨进程 flock、内核等待者、可重入检测）；strata 的写者只有 daemon 内的单写连接，其排队性质见 daemon 的改写行 |
| layerfs-persistence/tests/writer_admission.rs::a_holder_process_that_exits_hands_the_lock_to_the_waiter | 持有锁的进程退出（被杀）后，内核释放其锁，等待者随即取得写入权，不留任何残余（仅 Linux 编译） | store | 改写 | 改写为 store 的崩溃释放用例：持有写锁的进程被 SIGKILL 后，另一进程立刻能写，不留陈旧锁（SQLite 的 POSIX/WAL 锁由内核回收）；flock 与 Linux 限定不保留 |
| layerfs-persistence/tests/writer_admission.rs::a_signalled_kernel_waiter_is_cancelled_and_leaves_the_queue_cleanly | 被信号打断的内核等待者（EINTR）被取消并干净地退出队列，下一个等待者接手（仅 Linux 编译） | — | 不保留 | 实现细节（协作式写者准入）：LayerFS 在 SQLite BEGIN 之前自己做的准入（共享 session、跨进程 flock、内核等待者、可重入检测）；strata 的写者只有 daemon 内的单写连接，其排队性质见 daemon 的改写行 |
| layerfs-persistence/tests/writer_admission.rs::a_thread_reentering_its_own_turn_is_refused_instead_of_deadlocking | 线程在自己的回合内重入时被拒（Malformed）而不是死锁；回合释放后下一次调用立即被准入 | — | 不保留 | 实现细节（协作式写者准入）：LayerFS 在 SQLite BEGIN 之前自己做的准入（共享 session、跨进程 flock、内核等待者、可重入检测）；strata 的写者只有 daemon 内的单写连接，其排队性质见 daemon 的改写行 |

总计: 692

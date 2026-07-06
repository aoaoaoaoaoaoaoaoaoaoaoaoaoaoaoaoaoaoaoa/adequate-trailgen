\* trailgen_loop_milp start_v3 *\
Minimize
 obj: + 0.112914563 z_e0_v0_v1 + 0.112914563 z_e0_v1_v0 + 0.238867552 z_e1_v1_v2 + 0.238867552 z_e1_v2_v1 + 0.119236994 z_e2_v2_v3 + 0.119236994 z_e2_v3_v2 + 63.637158038 z_e3_v3_v0 + 63.637158038 z_e3_v0_v3 + 67.074311002 z_e4_v3_v1 + 67.074311002 z_e4_v1_v3 + 10.127239944 z_e5_v2_v4 + 10.127239944 z_e5_v4_v2
Subject To
 out_degree_v0: + 1 z_e0_v0_v1 + 1 z_e3_v0_v3 - 1 y_v0 = 0
 in_degree_v0: + 1 z_e0_v1_v0 + 1 z_e3_v3_v0 - 1 y_v0 = 0
 out_degree_v1: + 1 z_e0_v1_v0 + 1 z_e1_v1_v2 + 1 z_e4_v1_v3 - 1 y_v1 = 0
 in_degree_v1: + 1 z_e0_v0_v1 + 1 z_e1_v2_v1 + 1 z_e4_v3_v1 - 1 y_v1 = 0
 out_degree_v2: + 1 z_e1_v2_v1 + 1 z_e2_v2_v3 + 1 z_e5_v2_v4 - 1 y_v2 = 0
 in_degree_v2: + 1 z_e1_v1_v2 + 1 z_e2_v3_v2 + 1 z_e5_v4_v2 - 1 y_v2 = 0
 out_degree_v3: + 1 z_e2_v3_v2 + 1 z_e3_v3_v0 + 1 z_e4_v3_v1 - 1 y_v3 = 0
 in_degree_v3: + 1 z_e2_v2_v3 + 1 z_e3_v0_v3 + 1 z_e4_v1_v3 - 1 y_v3 = 0
 out_degree_v4: + 1 z_e5_v4_v2 - 1 y_v4 = 0
 in_degree_v4: + 1 z_e5_v2_v4 - 1 y_v4 = 0
 force_start: + 1 y_v3 = 1
 flow_gate_e0_v0_v1: + 1 f_e0_v0_v1 - 4 z_e0_v0_v1 <= 0
 flow_gate_e0_v1_v0: + 1 f_e0_v1_v0 - 4 z_e0_v1_v0 <= 0
 flow_gate_e1_v1_v2: + 1 f_e1_v1_v2 - 4 z_e1_v1_v2 <= 0
 flow_gate_e1_v2_v1: + 1 f_e1_v2_v1 - 4 z_e1_v2_v1 <= 0
 flow_gate_e2_v2_v3: + 1 f_e2_v2_v3 - 4 z_e2_v2_v3 <= 0
 flow_gate_e2_v3_v2: + 1 f_e2_v3_v2 - 4 z_e2_v3_v2 <= 0
 flow_gate_e3_v3_v0: + 1 f_e3_v3_v0 - 4 z_e3_v3_v0 <= 0
 flow_gate_e3_v0_v3: + 1 f_e3_v0_v3 - 4 z_e3_v0_v3 <= 0
 flow_gate_e4_v3_v1: + 1 f_e4_v3_v1 - 4 z_e4_v3_v1 <= 0
 flow_gate_e4_v1_v3: + 1 f_e4_v1_v3 - 4 z_e4_v1_v3 <= 0
 flow_gate_e5_v2_v4: + 1 f_e5_v2_v4 - 4 z_e5_v2_v4 <= 0
 flow_gate_e5_v4_v2: + 1 f_e5_v4_v2 - 4 z_e5_v4_v2 <= 0
 flow_start_supplies_visited_vertices: - 1 f_e2_v2_v3 + 1 f_e2_v3_v2 + 1 f_e3_v3_v0 - 1 f_e3_v0_v3 + 1 f_e4_v3_v1 - 1 f_e4_v1_v3 - 1 y_v0 - 1 y_v1 - 1 y_v2 - 1 y_v4 = 0
 flow_v0_receives_if_visited: - 1 f_e0_v0_v1 + 1 f_e0_v1_v0 + 1 f_e3_v3_v0 - 1 f_e3_v0_v3 - 1 y_v0 = 0
 flow_v1_receives_if_visited: + 1 f_e0_v0_v1 - 1 f_e0_v1_v0 - 1 f_e1_v1_v2 + 1 f_e1_v2_v1 + 1 f_e4_v3_v1 - 1 f_e4_v1_v3 - 1 y_v1 = 0
 flow_v2_receives_if_visited: + 1 f_e1_v1_v2 - 1 f_e1_v2_v1 - 1 f_e2_v2_v3 + 1 f_e2_v3_v2 - 1 f_e5_v2_v4 + 1 f_e5_v4_v2 - 1 y_v2 = 0
 flow_v4_receives_if_visited: + 1 f_e5_v2_v4 - 1 f_e5_v4_v2 - 1 y_v4 = 0
 distance_min: + 1021.984821548 z_e0_v0_v1 + 1021.984821548 z_e0_v1_v0 + 1334.340962802 z_e1_v1_v2 + 1334.340962802 z_e1_v2_v1 + 1022.16447978 z_e2_v2_v3 + 1022.16447978 z_e2_v3_v2 + 1334.340962802 z_e3_v3_v0 + 1334.340962802 z_e3_v0_v3 + 1680.804095031 z_e4_v3_v1 + 1680.804095031 z_e4_v1_v3 + 681.44298652 z_e5_v2_v4 + 681.44298652 z_e5_v4_v2 >= 3000
 distance_max: + 1021.984821548 z_e0_v0_v1 + 1021.984821548 z_e0_v1_v0 + 1334.340962802 z_e1_v1_v2 + 1334.340962802 z_e1_v2_v1 + 1022.16447978 z_e2_v2_v3 + 1022.16447978 z_e2_v3_v2 + 1334.340962802 z_e3_v3_v0 + 1334.340962802 z_e3_v0_v3 + 1680.804095031 z_e4_v3_v1 + 1680.804095031 z_e4_v1_v3 + 681.44298652 z_e5_v2_v4 + 681.44298652 z_e5_v4_v2 <= 8000
 difficulty_min: + 1.645100374 z_e0_v0_v1 + 1.645100374 z_e0_v1_v0 + 4.777351037 z_e1_v1_v2 + 4.777351037 z_e1_v2_v1 + 1.7714412 z_e2_v2_v3 + 1.7714412 z_e2_v3_v2 + 1005.874968201 z_e3_v3_v0 + 1005.874968201 z_e3_v0_v3 + 1004.316918575 z_e4_v3_v1 + 1004.316918575 z_e4_v1_v3 + 52.627341846 z_e5_v2_v4 + 52.627341846 z_e5_v4_v2 >= 0
 difficulty_max: + 1.645100374 z_e0_v0_v1 + 1.645100374 z_e0_v1_v0 + 4.777351037 z_e1_v1_v2 + 4.777351037 z_e1_v2_v1 + 1.7714412 z_e2_v2_v3 + 1.7714412 z_e2_v3_v2 + 1005.874968201 z_e3_v3_v0 + 1005.874968201 z_e3_v0_v3 + 1004.316918575 z_e4_v3_v1 + 1004.316918575 z_e4_v1_v3 + 52.627341846 z_e5_v2_v4 + 52.627341846 z_e5_v4_v2 <= 90
 ascent_min: + 0 z_e0_v0_v1 + 40.8 z_e0_v1_v0 + 0 z_e1_v1_v2 + 96.2 z_e1_v2_v1 + 24 z_e2_v2_v3 + 0 z_e2_v3_v2 + 113 z_e3_v3_v0 + 0 z_e3_v0_v3 + 72.2 z_e4_v3_v1 + 0 z_e4_v1_v3 + 0 z_e5_v2_v4 + 16 z_e5_v4_v2 >= 0
 ascent_max: + 0 z_e0_v0_v1 + 40.8 z_e0_v1_v0 + 0 z_e1_v1_v2 + 96.2 z_e1_v2_v1 + 24 z_e2_v2_v3 + 0 z_e2_v3_v2 + 113 z_e3_v3_v0 + 0 z_e3_v0_v3 + 72.2 z_e4_v3_v1 + 0 z_e4_v1_v3 + 0 z_e5_v2_v4 + 16 z_e5_v4_v2 <= 3000
 descent_min: + 40.8 z_e0_v0_v1 + 0 z_e0_v1_v0 + 96.2 z_e1_v1_v2 + 0 z_e1_v2_v1 + 0 z_e2_v2_v3 + 24 z_e2_v3_v2 + 0 z_e3_v3_v0 + 113 z_e3_v0_v3 + 0 z_e4_v3_v1 + 72.2 z_e4_v1_v3 + 16 z_e5_v2_v4 + 0 z_e5_v4_v2 >= 0
 descent_max: + 40.8 z_e0_v0_v1 + 0 z_e0_v1_v0 + 96.2 z_e1_v1_v2 + 0 z_e1_v2_v1 + 0 z_e2_v2_v3 + 24 z_e2_v3_v2 + 0 z_e3_v3_v0 + 113 z_e3_v0_v3 + 0 z_e4_v3_v1 + 72.2 z_e4_v1_v3 + 16 z_e5_v2_v4 + 0 z_e5_v4_v2 <= 3000
 road_fraction_max: - 91.978633939 z_e0_v0_v1 - 91.978633939 z_e0_v1_v0 - 160.120915536 z_e1_v1_v2 - 160.120915536 z_e1_v2_v1 - 91.99480318 z_e2_v2_v3 - 91.99480318 z_e2_v3_v2 - 160.120915536 z_e3_v3_v0 - 160.120915536 z_e3_v0_v3 - 151.272368553 z_e4_v3_v1 - 151.272368553 z_e4_v1_v3 + 599.669828137 z_e5_v2_v4 + 599.669828137 z_e5_v4_v2 <= 0
 low_confidence_fraction_max: - 204.39696431 z_e0_v0_v1 - 204.39696431 z_e0_v1_v0 - 266.86819256 z_e1_v1_v2 - 266.86819256 z_e1_v2_v1 - 204.432895956 z_e2_v2_v3 - 204.432895956 z_e2_v3_v2 - 266.86819256 z_e3_v3_v0 - 266.86819256 z_e3_v0_v3 - 336.160819006 z_e4_v3_v1 - 336.160819006 z_e4_v1_v3 - 136.288597304 z_e5_v2_v4 - 136.288597304 z_e5_v4_v2 <= 0
 restricted_access_fraction_max: + 0 z_e0_v0_v1 + 0 z_e0_v1_v0 + 0 z_e1_v1_v2 + 0 z_e1_v2_v1 + 0 z_e2_v2_v3 + 0 z_e2_v3_v2 + 1334.340962802 z_e3_v3_v0 + 1334.340962802 z_e3_v0_v3 + 1680.804095031 z_e4_v3_v1 + 1680.804095031 z_e4_v1_v3 + 681.44298652 z_e5_v2_v4 + 681.44298652 z_e5_v4_v2 <= 0
Bounds
 0 <= f_e0_v0_v1
 0 <= f_e0_v1_v0
 0 <= f_e1_v1_v2
 0 <= f_e1_v2_v1
 0 <= f_e2_v2_v3
 0 <= f_e2_v3_v2
 0 <= f_e3_v3_v0
 0 <= f_e3_v0_v3
 0 <= f_e4_v3_v1
 0 <= f_e4_v1_v3
 0 <= f_e5_v2_v4
 0 <= f_e5_v4_v2
Binary
 y_v0
 y_v1
 y_v2
 y_v3
 y_v4
 z_e0_v0_v1
 z_e0_v1_v0
 z_e1_v1_v2
 z_e1_v2_v1
 z_e2_v2_v3
 z_e2_v3_v2
 z_e3_v0_v3
 z_e3_v3_v0
 z_e4_v1_v3
 z_e4_v3_v1
 z_e5_v2_v4
 z_e5_v4_v2
End

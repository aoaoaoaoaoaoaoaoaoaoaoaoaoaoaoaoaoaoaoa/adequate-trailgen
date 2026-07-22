# US Public Trail Source Census

Assessed 2026-07-22. This is an acquisition ledger, not a claim that Trailgen currently consumes
every source below. The runtime automatic corpus remains OSM plus USGS National Digital Trails.
Of the 50 state authorities, 33 expose a machine-readable candidate, two are blocked by reuse
restrictions, and 15 require heavier extraction.

## Status Language

- **Machine**: an authority-owned, public line service or download can be queried without a token.
  It still needs a field adapter, license adjudication, and a coverage oracle before automatic use.
- **Restricted**: machine geometry exists, but access controls or stated reuse terms currently bar
  unattended product ingestion.
- **Extract**: the authority publishes trail maps, an interactive viewer, or evidence of an internal
  GIS corpus, but no stable public line service was found. PDF/vector-tile/application extraction is
  technically plausible and deserves a separate provenance and terms review.

"Completeness" is relative to the managing authority's own terrestrial walking network. It does not
mean every public trail in the state. Statewide greenway, transport, snowmobile, paddle, and grant
inventories are called incomplete when they omit ordinary state-park footpaths or mix proposed lines
with existing trails.

## State Authorities

| State | Status | Best authority corpus | Completeness and acquisition risk |
| --- | --- | --- | --- |
| Alabama | Machine | ADCNR [DCNR Trails](https://conservationgis.alabama.gov/adcnrweb/rest/services/DCNRTrails/MapServer/0) | Medium. Official queryable trail lines; verify park coverage and field domains. |
| Alaska | Machine | DNR [State Park Trail](https://arcgis.dnr.alaska.gov/arcgis/rest/services/OpenData/Recreational_ParkBoundary/MapServer/1); AGO [SCORP Trails](https://services1.arcgis.com/7HDiw78fcUiM2BWn/arcgis/rest/services/SCORP_Trails_/FeatureServer) | High for state parks, medium statewide. SCORP is a dated cross-jurisdiction snapshot. |
| Arizona | Machine | State Parks [TRAZ Trails of Arizona](https://services2.arcgis.com/gdcQ6sUWKP8qwBmV/arcgis/rest/services/TRAZ_Trails_of_Arizona/FeatureServer) | Medium-high. Statewide and multi-jurisdictional; current/proposed and access fields need auditing. |
| Arkansas | Machine | Arkansas GIS Office [Trails layer](https://gis.arkansas.gov/arcgis/rest/services/FEATURESERVICES/Environment/FeatureServer/22) | Medium. Official statewide service, but park-by-park recall has not been measured. |
| California | Restricted | State Parks [CSP Roads and Trails](https://services2.arcgis.com/AhxrK3F6WM8ECvDi/arcgis/rest/services/CSP_Roads_and_Trails_WGS84/FeatureServer) | Geometry is statewide but dated 2021. Terms prohibit alteration or sale and require advance approval for commercial use. District services are newer but fragmentary. |
| Colorado | Machine | CPW [COTREX Trails](https://services5.arcgis.com/ttNGmDvKQA7oeDQ3/ArcGIS/rest/services/CPWAdminData/FeatureServer/15) | High. Strong statewide multi-manager corpus; source authority and access semantics still need preservation. |
| Connecticut | Machine | DEEP [Trails Set](https://services1.arcgis.com/FjPcSmEFuDYlIdKC/arcgis/rest/services/DEEP_Trails_Set/FeatureServer) | High for DEEP lands. Check update cadence and municipal omissions. |
| Delaware | Machine | FirstMap [Trails and Pathways](https://enterprise.firstmap.delaware.gov/arcgis/rest/services/Transportation/DE_Trails_and_Pathways/FeatureServer) | Medium. Transportation framing may omit small natural-surface park paths. |
| Florida | Machine | DEP [Existing Trails](https://ca.dep.state.fl.us/arcgis/rest/services/OpenData/OGT/MapServer/3); FDACS [State Forest Trails](https://services3.arcgis.com/XYg2eF8UuxZVuVmF/arcgis/rest/services/Recreation_Trails/FeatureServer) | Medium-high only after union. DEP and state-forest stewardship are split. |
| Georgia | Extract | Georgia State Parks park guide and per-park maps | Low confidence. No statewide DNR line service surfaced; inventory and extract authoritative park PDFs. |
| Hawaii | Machine | DLNR [Na Ala Hele](https://geodata.hawaii.gov/arcgis/rest/services/Terrestrial/MapServer/34) | Medium. Authoritative state trail/access system, not necessarily every park footpath. Island and closure semantics matter. |
| Idaho | Machine | State GIS [Idaho Recreation Trails](https://services1.arcgis.com/CNPdEkvnGl65jCX8/arcgis/rest/services/Idaho_Recreation_Trails/FeatureServer) | Medium-high. Statewide, but managing authority and present access require filtering. |
| Illinois | Extract | IDNR hiking directory and per-site maps | Low. The authority advertises roughly 270 trails and 700 miles but exposes no statewide line corpus found in this sweep. |
| Indiana | Machine | IndianaMap [DNR Trails](https://gisdata.in.gov/server/rest/services/Hosted/Trails_AGOL_RO/FeatureServer/0) | High for DNR lands. Straightforward feature-service adapter. |
| Iowa | Machine | Iowa DOT [Trail View](https://services.arcgis.com/8lRhdTsQyJpO52F1/arcgis/rest/services/Trail_View/FeatureServer) | Low-medium for hiking. Strong recreational-transport inventory, likely incomplete for small park paths. |
| Kansas | Extract | KDWP park maps; legacy Rec-Finder geodatabase | Low. The only statewide GIS inventory found was an old planning artifact; current authority material is park-page/PDF oriented. |
| Kentucky | Machine | Kentucky GeoNet [Recreational Trails](https://services3.arcgis.com/ghsX9CKghMvyYjBU/arcgis/rest/services/Ky_Recreational_Trails/FeatureServer) | Medium-high. Broad official corpus; distinguish source-native records from USGS-derived copies. |
| Louisiana | Extract | State Parks document archive and park maps | Low. No statewide park trail line service found; public maps and the SCORP inventory are the heavier-extraction path. |
| Maine | Extract | DACF park trail pages and maps | Low. Rich descriptions and maps, but no Bureau of Parks and Lands statewide line service found. Maine Trail Finder is useful corroboration, not the authority. |
| Maryland | Machine | iMAP [Recreational Land Trails](https://mdgeodata.md.gov/imap/rest/services/Society/MD_RecreationalUses/FeatureServer/26) | Medium-high. Official statewide layer; validate allowed-use and closure currency. |
| Massachusetts | Machine | MassGIS [Hiking Trails for NextGen 911](https://services1.arcgis.com/hGdibHYSPO59RG1h/arcgis/rest/services/Hiking_Trails_for_NextGen_911/FeatureServer) | Medium-high. Excellent geometry, but emergency-cartography purpose may include non-public or omit informal recreational lines. |
| Michigan | Machine | DNR [Hiking Trails](https://gisagodnr.state.mi.us/arcgis/rest/services/DNR/DNRTrailsOPENDATA/FeatureServer/2) | High for DNR holdings. |
| Minnesota | Machine | DNR [State Park Trails and Roads](https://enterprise.gisdata.mn.gov/aghost/rest/services/us_mn_state_dnr/trans_state_park_trails_roads/FeatureServer) | High for state parks. Roads and trails require semantic separation. |
| Mississippi | Extract | MDWFP SCORP state-park trail tables and per-park maps | Low. The plan inventories trails but no reusable statewide geometry endpoint surfaced. |
| Missouri | Machine | Department of Conservation [Trails](https://services2.arcgis.com/kNS2ppBA4rwAQQZy/arcgis/rest/services/MO_Missouri_Department_of_Conservation_Trails/FeatureServer) | Medium-low statewide. Strong for conservation areas, suspected incomplete for State Parks and other managers. |
| Montana | Machine | FWP [Montana State Parks Trails](https://services3.arcgis.com/Cdxz8r11hT0MGzg1/arcgis/rest/services/Montana_State_Parks_Trails/FeatureServer) | High for state parks. |
| Nebraska | Machine | Game and Parks [Park Trails](https://services5.arcgis.com/IOshH1zLrIieqrNk/arcgis/rest/services/Park_Trails/FeatureServer/0) | High. Hidden behind an ArcGIS Experience, but the authority also advertises an open-data portal. Terms say reference/display use, so reuse wording needs adjudication. |
| Nevada | Extract | Nevada State Parks per-park PDF maps | Low. No statewide park line service found; BLM covers much of the state but is not a substitute for the state authority. |
| New Hampshire | Machine | NH GRANIT [Recreational Trails](https://nhgeodata.unh.edu/hosting/rest/services/Hosted/CSD_RecreationResources/FeatureServer/2) | Medium-high. Official statewide clearinghouse; audit age and managing authority. |
| New Jersey | Machine | NJDEP [Statewide Trails](https://services1.arcgis.com/QWdNfRs7lkPq4g4Q/arcgis/rest/services/Statewide_Trails_in_New_Jersey/FeatureServer); [State Park Service Trails](https://mapsdep.nj.gov/arcgis/rest/services/Features/Land/MapServer/63) | High. Prefer the park layer as authority and the statewide layer as corroborating geometry. |
| New Mexico | Extract | EMNRD SCORP `NM_State_Park_Trails` inventory; NMDOT [Trails](https://services.arcgis.com/hOpd7wfnKm16p9D9/arcgis/rest/services/Trails/FeatureServer) | Low-medium. The SCORP proves a state-park line layer exists, but no stable public download was found; NMDOT is not a complete substitute. |
| New York | Restricted | OPRHP [NY State Parks Trails](https://services.arcgis.com/1xFZPtKn1wKC6POA/arcgis/rest/services/NY_State_Parks_Trails/FeatureServer) | High geometry and attributes, including marked/unmarked, but current item terms limit use to informational/non-commercial purposes. Keep catalogued, not automatic. |
| North Carolina | Extract | Division of Parks and Recreation per-park maps; NC OneMap coastal paddle trails | Low. The public state service found is for paddle trails, not the terrestrial park network. |
| North Dakota | Machine | Parks and Recreation [Statewide Trails](https://services3.arcgis.com/G9FKQ0xH9VagrUos/arcgis/rest/services/North_Dakota_Statewide_Trails/FeatureServer) | Medium-high. Verify OuterSpatial-derived attributes and update cadence. |
| Ohio | Extract | ODNR park maps and internal statewide trail GIS | Medium-low. ODNR reports that it mapped its trail system, but no stable public trail line endpoint surfaced. A web-application extraction pass is warranted. |
| Oklahoma | Extract | TravelOK per-park maps and trail booklets | Low. No statewide state-park geometry service found; several parks publish separate trail maps. |
| Oregon | Machine | ODF [Protection Map Trails](https://gis.odf.oregon.gov/ags3/rest/services/Basemaps/ProtectionMap/MapServer/38) | Medium. Rich owner, closure, and source fields, but this is a mixed-agency cartographic layer rather than a clean OPRD publication. Audit age and park recall. |
| Pennsylvania | Machine | DCNR [State Park Trails](https://gis.dcnr.uat.pa.gov/agsstage/rest/services/StateParks/BSP_StateParksTrails/FeatureServer/0) | High for formal trails: field-surveyed, 30+ attributes, 119 parks, monthly updates. Endpoint is still on a UAT/stage host, so production stability is the principal risk. |
| Rhode Island | Machine | RIGIS [Hiking Trails on State Lands](https://services2.arcgis.com/S8zZg9pg23JUEexQ/arcgis/rest/services/TRANS_State_Hiking_Trails/FeatureServer) | High for state lands. |
| South Carolina | Extract | State Parks [Maps and Brochures](https://southcarolinaparks.com/maps-and-brochures) | Medium-low. Many vector-like trail PDFs exist, some with GPS provenance, but no statewide line service was found and some maps explicitly omit closed trails. |
| South Dakota | Extract | GFP park trail pages, PDFs, and interactive maps | Medium-low. The agency has a Parks GIS program and mapped products, but no stable public statewide line endpoint surfaced. |
| Tennessee | Machine | State Parks [Trails 2024](https://services5.arcgis.com/bPacKTm9cauMXVfn/arcgis/rest/services/Tennessee_State_Park_Trails_2024_(View_Only)/FeatureServer) | High for state parks. "View only" requires terms review, though the service is publicly queryable. |
| Texas | Machine | TPWD [Texas State Parks Trails](https://tpwd.texas.gov/arcgis/rest/services/Parks/TexasStateParksTrails/MapServer/0) and [statewide KMZ](https://tpwd.texas.gov/state-parks/park-information/maps/use-the-trails-maps-anytime-anywhere) | High for official state-park trails. The anonymous authority service supports paginated GeoJSON/PBF queries and park-vetted official/use fields; record attribution and the general TPWD data disclaimer in its adapter. |
| Utah | Machine | Utah [Recreational Trails and Pathways Network](https://services3.arcgis.com/17F7m6SrhCakwCcI/arcgis/rest/services/Utah_Recreational_Trails_and_Pathways_Network/FeatureServer) | Medium. Broad statewide network, but master-plan/proposed lines must be excluded from current routing. |
| Vermont | Machine | ANR [Tourism Trails](https://anrmaps.vermont.gov/arcgis/rest/services/Open_Data/OPENDATA_ANR_TOURISM_SP_NOCACHE_v2/MapServer/160) | Medium-high for ANR lands. |
| Virginia | Extract | Virginia State Parks GIS-team Avenza/PDF maps | Medium-low. The maps are georeferenced and likely derive from a coherent internal corpus, but no statewide public line service surfaced. |
| Washington | Machine | State Parks [Trails](https://services5.arcgis.com/4LKAHwqnBooVDUlX/arcgis/rest/services/Trails/FeatureServer); RCO [Statewide Trails](https://services2.arcgis.com/TGEC20q86HQAeMS6/arcgis/rest/services/WA_RCO_Trails_Database_Public_View/FeatureServer) | High after union. State Parks is authoritative for its holdings; RCO supplies broader multi-manager coverage. |
| West Virginia | Machine | DOT/RTI [West Virginia Trail Inventory](https://services2.arcgis.com/xLpB90lOmCXYDAWo/arcgis/rest/services/GTI_PUB_UTM_DBO_WV_Trail_Inventory/FeatureServer) | Medium-high. Broad funded inventory; currency and access semantics need audit. |
| Wisconsin | Machine | DNR [State Trail Lines](https://services5.arcgis.com/Ul9AyFFeFTjf08DW/arcgis/rest/services/PR_DISS_STATE_TRAIL_LN_24K/FeatureServer) | Low-medium for Trailgen. Authoritative state-trail corridors, but suspected incomplete for ordinary footpaths inside parks. |
| Wyoming | Extract | State Parks per-park brochures and maps | Low. No statewide terrestrial park-trail line service found; the snowmobile corpus and federal services do not fill that authority gap. |

## Federal Land Managers

| Authority | Status | Corpus | Completeness and acquisition risk |
| --- | --- | --- | --- |
| USGS | Automatic | [National Digital Trails](https://cartowfs.nationalmap.gov/arcgis/rest/services/transportation/MapServer/8) | Already ingested. It aggregates partner submissions and is not an access authority; originator identity must survive. |
| USDA Forest Service | Machine | [TrailNFS Publish](https://apps.fs.usda.gov/ArcX/rest/services/EDW/EDW_TrailNFSPublish_01/MapServer/0) | National and richly attributed. Completeness varies by forest: a forest may publish no data or only one of three attribute subsets. Highest-priority new adapter. |
| National Park Service | Machine | [NPS Public Trails](https://mapservices.nps.gov/arcgis/rest/services/NationalDatasets/NPS_Public_Trails_Geographic/FeatureServer/0) | Strong national public-use corpus with status, surface, type, use, season, and access fields. Restricted and sensitive lines are intentionally removed. Highest-priority new adapter. |
| Bureau of Land Management | Machine | [National GTLF Public Display](https://gis.blm.gov/arcgis/rest/services/transportation/BLM_Natl_GTLF_Public_Display/MapServer) | Strong western-US corpus. Ingest public non-motorized, non-mechanized, and managed layers without duplicating overlaps; preserve assessed/not-assessed and allowed-mode fields. |
| Fish and Wildlife Service | Machine | [FWS Trails Public View](https://services.arcgis.com/QVENGdaPbd4LUkLV/arcgis/rest/services/FWS_HQ_Trails_Cycle_3_Public_View/FeatureServer) | National refuge inventory using federal trail standards. Cycle 3 began in 2019 and was intended to finish in 2022; current completeness and refresh cadence need measurement. |
| Bureau of Reclamation | Machine | [Reclamation Trails, RISE item 133957](https://data.usbr.gov/catalog/6406/item/133957) | Annual western-US asset inventory with a RISE query API, not an ordinary ArcGIS layer. The catalog calls the public asset collection non-authoritative. Adapter needed. |
| Army Corps of Engineers | Extract | [National Recreation Sites](https://geospatial.sec.usace.army.mil/server/rest/services/Recreation/Recreation_Site/FeatureServer) plus district/project maps | The national service is points, not trail geometry. Trail maps are decentralized by district and project; use USGS/OSM until a MAPLand line corpus appears. |
| Tennessee Valley Authority | Extract | [TVA Trails Guide](https://www.tva.com/Environment/Recreation/TVA-Trails) | The authority publishes structured trail descriptions for about 190 miles, but no public national/regional route-geometry feed surfaced. Inspect application payloads and downloadable maps. |
| Department of Defense | Extract | Installation-level recreation maps | No national public trail-geometry corpus found. Public access is installation-specific and volatile; never infer it from geometry alone. |

Recreation.gov/RIDB supplies recreation-site records, not a canonical trail centerline network. National
Scenic and Historic Trail alignments are useful named-route overlays but do not replace the local
networks over which those routes run.

## Difficult And Suspect States

The first heavy-extraction queue should be **New Mexico, Ohio, South Dakota, and Virginia**. Each
authority demonstrably has or had a coherent GIS-backed trail application. Nebraska originally sat in
this tier but was resolved during this sweep by walking its ArcGIS Experience references to
`Park_Trails`; the same technique may recover these four without OCR or manual tracing.

The next queue is **Georgia, Illinois, Kansas, Louisiana, Maine, Mississippi, Nevada, North Carolina,
Oklahoma, South Carolina, and Wyoming**. These appear genuinely map-fragmented. Prefer embedded PDF
vectors or georeferenced Avenza layers, then OCR/legend classification, and only lastly traced raster
geometry. Preserve the originating document, page, publication date, extraction method, and geometric
error estimate as a raw provider receipt.

The legally difficult queue is **California and New York**. Both publish excellent geometry under
reuse terms unsuitable for an unattended general-purpose product. These are permission problems, not
parsing problems. Texas was initially misclassified from a dead ArcGIS catalog path; TPWD's own public
server and statewide GIS download establish a stable machine source.

Even among machine sources, assume incompleteness in **Iowa, Missouri, Oregon, Utah, and Wisconsin**:
their best statewide services are transport-oriented, manager-specific, mixed-source, proposal-bearing,
or corridor-only. **Alaska SCORP, Arizona, Arkansas, Delaware, Hawaii, New Hampshire, North Dakota, West
Virginia**, and the FWS corpus also need measured park-by-park recall and update-cadence checks before
receiving high trust.

## Admission Law

A catalog entry becomes an automatic provider only after it has:

1. compatible reuse terms recorded in code and receipts;
2. bounded, paginated acquisition with raw-response sequestration;
3. a provider-native field map for standing, marking, surface, access, allowed use, lifecycle, and IDs;
4. tests proving closed, private, proposed, motor-only, and historical lines cannot become ordinary
   walkable trails;
5. a state- or agency-specific coverage oracle against representative park inventories; and
6. duplicate suppression that retains authority provenance and never lets lower-priority geometry erase
   a stronger access or wayfinding claim.

Publicly viewable is not synonymous with licensed, current, complete, walkable, or open.

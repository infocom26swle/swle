use std::collections::{HashMap, VecDeque};
use std::slice::Iter;
use serde::{Deserialize, Serialize};
use anyhow::Result;
use tracing::{trace};

use crate::{
    crypto::{Digest, PublicKey, Signature},
};


/// SWLE 评分规则的常量
pub const ALPHA_0: f32 = -1.0;  // R1: 进入新视图时领导者扣分
pub const ALPHA_2: f32 = 1.0;   // R3: 提案最终确定时领导者加分
pub const ALPHA_4: f32 = 1.0;   // R5: 周期性归一化时所有副本加分


#[derive(Debug, Clone)]
pub struct VoterSet {
    pub voters: Vec<PublicKey>,
}

impl VoterSet {
    pub fn new(voters: Vec<PublicKey>) -> Self {
        Self { voters }
    }

    pub fn threshold(&self) -> usize {
        self.voters.len() - (self.voters.len() as f64 / 3.0).floor() as usize
    }

    pub fn iter(&self) -> Iter<PublicKey> {
        self.voters.iter()
    }

    pub fn get_voters(&self) -> Vec<PublicKey> {
        self.voters.clone()
    }
}

impl Iterator for VoterSet {
    type Item = PublicKey;

    fn next(&mut self) -> Option<Self::Item> {
        self.voters.pop()
    }
}

/// SWLE 领导者证书
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderCertificate {
    pub target_view: u64,
    pub selected_leader: Option<PublicKey>,
    pub votes: Vec<SWLEVote>,
}

/// SWLE 投票结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SWLEVote {
    pub voter: PublicKey,
    pub determined_leader: PublicKey,
    pub candidates: Vec<PublicKey>,
    pub block_hash: Digest,  // 添加这个字段
    pub signature: Signature,
}

/// SWLE 主要数据结构
#[derive(Debug, Clone)]
pub struct SWLE {
    /// 当前副本ID
    pub id: PublicKey,
    /// 投票者集合
    pub voter_set: VoterSet,
    /// 当前视图
    pub current_view: u64,
    /// 滑动窗口大小 Tz = ⌈Tf/n⌉ · n
    pub window_size: usize,
    /// 领导者列表，存储 [current_view, current_view + Tz + 2n - 1] 的领导者信息
    pub leader_list: HashMap<u64, LeaderInfo>,
    /// 声誉矩阵 S[replica_id] = score
    pub reputation_scores: HashMap<PublicKey, f32>,
    /// 周期性归一化计数器
    pub normalization_counter: u64,
    /// 归一化周期 Θ = max(200, 10n)
    pub theta: u64,
    // R2: 视图超时时领导者扣分
    pub alpha_1: f32,
    // R4: 成功完成视图时共识促进副本加分
    pub alpha_3: f32,
    // 已收集的投票
    pub collected_votes: HashMap<u64, Vec<SWLEVote>>,
}

/// 领导者信息
#[derive(Debug, Clone)]
pub struct LeaderInfo {
    /// 选举的领导者
    pub elected_leader: Option<PublicKey>,
    /// 初始领导者 (view mod n)
    pub initial_leader: PublicKey,
}

impl SWLE {
    /// 创建新的 SWLE 实例
    pub fn new(id: PublicKey, voter_set: VoterSet, tf: usize) -> Self {
        let n = voter_set.voters.len();
        let alpha_1: f32 = -1.0 * (n as f32);
        let alpha_3: f32 = 1.0 / (n as f32);
        // println!("{}",alpha_1);
        let window_size = ((tf + n - 1) / n) * n; // ⌈Tf/n⌉ · n
        let theta = std::cmp::max(300, 10 * n as u64);
        
        let mut leader_list = HashMap::new();
        let mut reputation_scores = HashMap::new();
        
        // 初始化声誉分数
        for voter in voter_set.iter() {
            reputation_scores.insert(*voter, 1.0);
        }
        
        // 初始化 LEADERLIST
        for view in 1..=(window_size + 2 * n) as u64 {
            let initial_leader = Self::get_initial_leader(view, &voter_set);
            let elected_leader = if view <= (window_size + n) as u64 {
                Some(initial_leader)
            } else {
                None
            };
            
            leader_list.insert(view, LeaderInfo {
                elected_leader,
                initial_leader,
            });
        }
        
        Self {
            id,
            voter_set,
            current_view: 1,
            window_size,
            leader_list,
            reputation_scores,
            normalization_counter: 0,
            theta,
            alpha_1,
            alpha_3,
            collected_votes: HashMap::new(),
        }
    }
    
    /// 获取初始领导者 (view mod n)
    pub fn get_initial_leader(view: u64, voter_set: &VoterSet) -> PublicKey {
        let index = ((view - 1) % voter_set.voters.len() as u64) as usize;
        voter_set.voters[index]
    }
    
    /// 获取当前视图的领导者
    pub fn get_current_leader(&self, view: u64) -> Option<PublicKey> {
        if let Some(leader_info) = self.leader_list.get(&view) {
            leader_info.elected_leader.or(Some(leader_info.initial_leader))
        } else {
            None
        }
    }
    
    /// 进入新视图时更新声誉分数 (规则R1)
    pub fn on_enter_view(&mut self, view: u64) {
        if let Some(leader) = self.get_current_leader(view) {
            self.update_reputation_score(leader, ALPHA_0);
        }
        self.current_view = view;
        self.normalization_counter += 1;
        
        // 检查是否需要周期性归一化 (规则R5)
        if self.normalization_counter % self.theta == 0 {
            self.periodic_normalization();
        }
        // trace!("{}, updated reputation scores (on entering view): {:?}", self.id, self.reputation_scores);
    }
    
    /// 视图超时时更新声誉分数 (规则R2)
    pub fn on_view_timeout(&mut self, view: u64) {
        if let Some(leader) = self.get_current_leader(view) {
            self.update_reputation_score(leader, self.alpha_1);
        }
        //trace!("{}, updated reputation scores (on view timeout): {:?}", self.id, self.reputation_scores);
    }
    
    /// 提案最终确定时更新声誉分数 (规则R3)
    pub fn on_proposal_finalized(&mut self, leader: PublicKey) {
        self.update_reputation_score(leader, ALPHA_2);
        //trace!("{}, updated reputation scores (on proposal finalized): {:?}", self.id, self.reputation_scores);
    }
    
    /// 成功完成视图时更新声誉分数 (规则R4)
    pub fn on_view_completed(&mut self, consensus_promoting_replicas: &[PublicKey]) {
        for replica in consensus_promoting_replicas {
            self.update_reputation_score(*replica, self.alpha_3);
        }
        //trace!("{}, updated reputation scores (on view completed): {:?}", self.id, self.reputation_scores);
    }
    
    /// 更新声誉分数
    fn update_reputation_score(&mut self, replica: PublicKey, delta: f32) {
        let current_score = self.reputation_scores.get(&replica).unwrap_or(&0.0);
        let new_score = f32::max(0.0, current_score + delta);
        self.reputation_scores.insert(replica, new_score);
    }
    
    /// 周期性归一化 (规则R5)
    fn periodic_normalization(&mut self) {
        for replica in self.voter_set.clone().iter() {
            self.update_reputation_score(*replica, ALPHA_4);
        }
    }
    
    /// 生成候选数组 (Algorithm 1)
    pub fn generate_candidates(&self, target_view: u64) -> Vec<PublicKey> {
        let mut candidates = Vec::new();
        let n = self.voter_set.voters.len();
        
        for i in 0..n {
            let view = target_view + i as u64;
            if let Some(leader_info) = self.leader_list.get(&view) {
                let candidate = leader_info.initial_leader;
                let score = self.reputation_scores.get(&candidate).unwrap_or(&0.0);
                
                if *score >= ALPHA_0.abs() {
                    candidates.push(candidate);
                }
            }
        }
        
        // 如果没有合格候选人，应用周期性归一化并重新计算
        if candidates.is_empty() {
            let mut temp_scores = self.reputation_scores.clone();
            for replica in self.voter_set.iter() {
                let current_score = temp_scores.get(replica).unwrap_or(&0.0);
                temp_scores.insert(*replica, current_score + ALPHA_4);
            }
            
            for i in 0..n {
                let view = target_view + i as u64;
                if let Some(leader_info) = self.leader_list.get(&view) {
                    let candidate = leader_info.initial_leader;
                    let score = temp_scores.get(&candidate).unwrap_or(&0.0);
                    
                    if *score >= ALPHA_0.abs() {
                        candidates.push(candidate);
                    }
                }
            }
        }
        
        candidates
    }
    
    /// 计算目标视图 (基于论文中的公式)
    pub fn compute_target_view(&self, current_view: u64) -> u64 {
        let n = self.voter_set.voters.len() as u64;
        let tz = self.window_size as u64;
        
        // Case (1): v + (⌊v/n⌋ mod n) ≤ ⌈v/n⌉ · n
        let condition1 = current_view + ((current_view / n) % n) <= ((current_view + n - 1) / n) * n;
        
        // Case (2): v/n ∈ Z ∧ (v/n) mod n = 1
        let condition2 = (current_view % n == 0) && ((current_view / n) % n == 1);
        
        if condition1 {
            current_view + n + tz + ((current_view / n) % n)
        } else if condition2 {
            current_view + n + tz
        } else {
            current_view + tz + (((current_view - 1) / n) % n)
        }
    }
    
    /// 候选选择和证明打包 (Algorithm 2)
    pub fn select_candidate_and_create_certificate(
        &self,
        votes: Vec<SWLEVote>,
        target_view: u64,
    ) -> LeaderCertificate {
        let mut count: HashMap<PublicKey, usize> = HashMap::new();
        let f = (self.voter_set.voters.len() - 1) / 3;
        
        // 收集候选数组并计数
        for vote in &votes {
            for candidate in &vote.candidates {
                *count.entry(*candidate).or_insert(0) += 1;
            }
        }
        
        // 找到获得 >= f + 1 票的候选人
        let mut eligible_candidates: Vec<PublicKey> = count
            .iter()
            .filter(|(_, &count)| count >= f + 1)
            .map(|(&candidate, _)| candidate)
            .collect();
        
        // 选择最早出现的候选人
        let selected_leader = if !eligible_candidates.is_empty() {
            // 按照在目标视图范围内最早出现的顺序排序
            eligible_candidates.sort_by_key(|&candidate| {
                (target_view..)
                    .find(|&v| {
                        self.leader_list
                            .get(&v)
                            .map(|info| info.initial_leader == candidate)
                            .unwrap_or(false)
                    })
                    .unwrap_or(u64::MAX)
            });
            
            Some(eligible_candidates[0])
        } else {
            None
        };
        
        // trace!("{}, LeaderCertificate is [{}, {:?}, {:?}]", self.id, target_view, selected_leader, votes);
        LeaderCertificate {
            target_view,
            selected_leader,
            votes,
        }
    }
    
    /// 更新 LEADERLIST (Algorithm 3)
    pub fn update_leader_list(&mut self, certificate: &LeaderCertificate) {
        let target_view = certificate.target_view;
        
        // 更新目标视图的领导者
        if let Some(leader_info) = self.leader_list.get_mut(&target_view) {
            leader_info.elected_leader = certificate.selected_leader
                .or(Some(leader_info.initial_leader));
        }
        
        // 填充中间视图的空隙
        let mut v_star = 0;
        for view in (1..target_view).rev() {
            if let Some(leader_info) = self.leader_list.get(&view) {
                if leader_info.elected_leader.is_some() {
                    v_star = view;
                    break;
                }
            }
        }
        
        for view in (v_star + 1)..target_view {
            if let Some(leader_info) = self.leader_list.get_mut(&view) {
                if leader_info.elected_leader.is_none() {
                    leader_info.elected_leader = Some(leader_info.initial_leader);
                }
            }
        }
       // trace!("{}, updated leader list for target view {}: {:?}", self.id, target_view, self.leader_list);
    }
    
    /// 扩展 LEADERLIST
    pub fn extend_leader_list(&mut self, new_view: u64) {
        let n = self.voter_set.voters.len() as u64;
        let new_target_view = new_view + self.window_size as u64 + 2 * n;
        
        if !self.leader_list.contains_key(&new_target_view) {
            let initial_leader = Self::get_initial_leader(new_target_view, &self.voter_set);
            self.leader_list.insert(new_target_view, LeaderInfo {
                elected_leader: None,
                initial_leader,
            });
        }
        
        // 移除过旧的视图
        self.leader_list.retain(|&view, _| view >= new_view);
        // trace!("{}, extended leader list to new view {}: {:?}", self.id, new_target_view, self.leader_list);
    }
    
    /// 收集投票
    pub fn add_vote(&mut self, vote: SWLEVote, target_view: u64) {
        self.collected_votes
            .entry(target_view)
            .or_insert_with(Vec::new)
            .push(vote);
    }
    
    /// 检查是否有足够的投票生成证书
    pub fn try_generate_certificate(&self, target_view: u64) -> Option<LeaderCertificate> {
        if let Some(votes) = self.collected_votes.get(&target_view) {
            let threshold = 2 * (self.voter_set.voters.len() - 1) / 3 + 1;
            if votes.len() >= threshold {
                return Some(self.select_candidate_and_create_certificate(
                    votes.clone(), 
                    target_view
                ));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{generate_keypairs, PublicKey};
    
    fn create_test_voter_set(n: usize) -> VoterSet {
        let keypairs = generate_keypairs(n);
        let voters: Vec<PublicKey> = keypairs.into_iter().map(|(pk, _)| pk).collect();
        VoterSet::new(voters)
    }
    
    #[test]
    fn test_swle_initialization() {
        let voter_set = create_test_voter_set(4);
        let id = voter_set.voters[0];
        let swle = SWLE::new(id, voter_set, 100);
        
        assert_eq!(swle.current_view, 1);
        assert_eq!(swle.reputation_scores.len(), 4);
        assert!(swle.leader_list.len() > 0);
    }
    
    #[test]
    fn test_reputation_scoring() {
        let voter_set = create_test_voter_set(4);
        let id = voter_set.voters[0];
        let mut swle = SWLE::new(id, voter_set.clone(), 100);
        
        let leader = voter_set.voters[0];
        
        // 测试规则 R1: 进入新视图
        swle.on_enter_view(1);
        assert_eq!(swle.reputation_scores[&leader], 0.0);
        
        // 测试规则 R3: 提案最终确定
        swle.on_proposal_finalized(leader);
        assert_eq!(swle.reputation_scores[&leader], 1.0);
        
        // 测试规则 R2: 视图超时
        swle.on_view_timeout(1);
        let expected_score = f32::max(0.0, 0.0 + swle.alpha_1); // 应该是 0，因为不能为负
        assert_eq!(swle.reputation_scores[&leader], expected_score);
    }
    
    #[test]
    fn test_candidate_generation() {
        let voter_set = create_test_voter_set(4);
        let id = voter_set.voters[0];
        let mut swle = SWLE::new(id, voter_set.clone(), 100);
        
        // 给所有节点一些正分数
        for voter in voter_set.iter() {
            swle.update_reputation_score(*voter, 5.0);
        }
        
        let candidates = swle.generate_candidates(10);
        assert!(candidates.len() > 0);
        assert!(candidates.len() <= 4);
    }
    
    #[test]
    fn test_target_view_computation() {
        let voter_set = create_test_voter_set(4);
        let id = voter_set.voters[0];
        let swle = SWLE::new(id, voter_set, 4);
        
        let target = swle.compute_target_view(1);
        assert_eq!(target, 9);
        
        let target2 = swle.compute_target_view(5);
        assert_eq!(target2, 14);
    }
    
    #[test]
    fn test_leader_list_update() {
        let voter_set = create_test_voter_set(4);
        let id = voter_set.voters[0];
        let mut swle = SWLE::new(id, voter_set.clone(), 100);
        
        let certificate = LeaderCertificate {
            target_view: 10,
            selected_leader: Some(voter_set.voters[1]),
            votes: vec![],
        };
        
        swle.update_leader_list(&certificate);
        
        if let Some(leader_info) = swle.leader_list.get(&10) {
            assert_eq!(leader_info.elected_leader, Some(voter_set.voters[1]));
        }
    }
    
    #[test]
    fn test_periodic_normalization() {
        let voter_set = create_test_voter_set(4);
        let id = voter_set.voters[0];
        let mut swle = SWLE::new(id, voter_set.clone(), 100);
        
        // 设置一个较小的 theta 用于测试
        swle.theta = 2;
        
        let initial_scores: Vec<f32> = voter_set.voters.iter()
            .map(|v| swle.reputation_scores[v])
            .collect();
        
        // 进入视图触发归一化
        swle.on_enter_view(1);
        swle.on_enter_view(2);
        
        let after_scores: Vec<f32> = voter_set.voters.iter()
            .map(|v| swle.reputation_scores[v])
            .collect();
        
        // 应该有一些分数发生了变化（由于归一化）
        assert_ne!(initial_scores, after_scores);
    }
}